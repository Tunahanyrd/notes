# notes-preview: pure-Rust previews and thumbnails

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Port of `samsung_notes/previews.py`, `thumbnails.py`, and the preparation they drive: `pdf_background`,
`ink_render`/`ink_render_job`, `preview_codec` and `page_image`. The crate has no PyO3 calls and starts no
Python child for preparation.

A test binary that links it was checked with `ldd`: no libpython. `nm -u` shows no `Py*` symbols.

## Layout

- `manager.rs`: `PreviewManager`. It covers `_source`, `_entry`, `_ink_inventory`, `manifest`, `_path`,
  `request`/`_request`, `prefetch`, `_cache_png`, the worker loop and guard, `close`/`join`, and
  `template_pdf` (used by `apply_template`). It also provides the job table, the priority queue with its
  sequence tiebreak and `task_done`/`join`, and the Condvar wait.
- `thumbnails.rs`: `ThumbnailManager`, `thumbnail_size`, `downsample_page` and the `PagePreviews` trait.
- `pdf.rs`: `inspect_pdf_backgrounds` (over a parsed payload, or raw input), `pdf_page_indices`,
  `cached_pdf`, `fetch_pdf` / `fetch_pdf_with`, `render_pdf_background`, `compose_pdf_canvas`,
  `light_from_dark` and `rgba`.
- `fetch.rs`: `_url` and `fetch_preview` / `fetch_preview_with`.
- `payload.rs`: `_payload_from_snapshot` and `_inventory_from_snapshot` (rs-doc and rs-ink core).
- `codec.rs`: `preview_codec.provider_available` / `decode_spi`, and the default `_codec` decoder, backed by
  `spi/` (below).
- `spi/`: independent pure-Rust canvas-cache (SPI) decoder (`decode`, `decode_with_stats`, `parse_header`);
  format in docs/CANVAS_CACHE.md. Tests: `tests/spi.rs` with constructed fixtures in `tests/fixtures/spi/`.
- `ink/prepare.rs`: `_prepare_pages` in pure Rust, using `notes_host::PureDeps`, the rs-ops `verify`, rs-ink
  decode/bind and rs-editor `object_rich`.
- `ink/mod.rs`: `inspect_render_pages`, `render_ink_page`, the per-note body cache, and `run_child`.
- `env.rs`: `PreviewEnv`, the engine boundary. It supplies the session path, the offline flag, account
  headers, the `http`/`download` transport, the project root, and the self-exec executable.

Pure helpers reused from rs-render are `pixels`, `raster`, `sandbox` and `pdf_layout`. From its pdf and ink
code it reuses `valid_cached_pdf`, `cache_name`, `render_page_pixels`, `run_renderer`, `renderer_available`,
and `allowed_url`.

## Process boundaries

- **Ink preparation runs in-process by default.** A thread per note fills the same body cache (96 MiB,
  4 notes) that the PyO3 streaming child fills.
- **Self-exec is optional.** When `PreviewEnv::ink_child_executable()` returns a path, the engine binary is
  re-executed as `<exe> __notes-ink-render-job inventory|render <ppid> [page]`, and `main()` must call
  `notes_preview::run_child(&argv)` first. The child gets the same isolation as the Python child:
  - its own session, killed at 180 s;
  - PDEATHSIG;
  - RLIMIT_AS 3 GiB, RLIMIT_CPU 170 s, CORE 0.
- **In-process trade-off.** In-process mode has no rlimits or hard kill. Waiters still give up after 185 s.
- **External tools are unchanged from rs-render.** pdftoppm, the GPL ink renderer and rsvg-convert keep
  their bwrap sandbox, rlimits and PDEATHSIG.
- **macOS:** no bwrap; `rs_render::sandbox::{pdf_args,ink_args}` run the same tools under `env -i` + Seatbelt (`notes_platform::isolation`) with the same rlimits minus `RLIMIT_AS`, and no PDEATHSIG (docs/MACOS_PORT.md).
- **Canvas caches are decoded in-process by our own decoder** (`spi/`, since 2026-10-27). It replaces the former
  Python child that ran the user's SHA256-pinned native library under Unicorn; no Samsung binary, emulator or
  interpreter is involved, so `provider_available()` is always true. Bounds: input 32 MiB (8 MiB before 2026-10-28), 4 Mi pixels, header
  checked before any allocation, no partial images, malformed input is an error (never a panic). The PNG is
  produced by the same encoder as before (`rs_render::pixels::rgba_png`), so output bytes are unchanged and the
  recipe key stays `native-preview-v1`. The historical glue is in `reference/spi-emulator-glue/`.
- **No HTTP code is in this crate.** The network goes through `PreviewEnv::http` / `download`, and the
  default for both is "no network".

## Divergences (edge cases)

- **Where headers are requested.** `engine._api()` is called through `env.headers()` at the point where
  `api._headers()` is read, which is after the URL allowlist check, not before it. Only the error class
  differs, and only when both the URL check and the API call fail.
- **Persisted ink inventories are type-checked.** Files with non-integer index, width or height are
  recomputed. Python accepts any values as long as the keys match.
- **Weak-random fallback.** Temporary-name randomness falls back to time/pid hashing if getrandom fails.
- **Rasterizer.** Ink PNGs come from resvg by default, as in rs-render. With `SAMSUNG_NOTES_INK_RASTER=rsvg`
  they are byte-identical to pure Python.
- **Shape/line pages (intended, 2026-10-27).** Recognised shapes (`rs_ops::page_shapes::renderer_skippable_shape`,
  exact attribute allowlist) are left out of the renderer input and a shape-only page renders as a plain
  background, exactly as rs-render does (see crates/rs-render/PORTING.md). Pure Python still refuses such
  pages. Tests: `tests/ink.rs` shape cases.
- **Stylus-less InkPen2 strokes (intended, 2026-10-27).** The Python admission (`_admitted_pen`) required the
  stylus-axis bit on every InkPen2 stroke. Our own writer (rs-ink recipes 2/3) copies the template's tool 2 but
  stores no tilt/orientation and clears that bit, so a page with one stroke drawn on Linux left the inventory and,
  its Samsung canvas cache being released by the write, showed nothing. Such strokes (tool 2, every other gate
  unchanged) are now admitted; the renderer draws them through upstream's InkPen2 approximation (its V9 route needs
  the axes), the path the live pen preview (crates/notes-ink-live) already draws. Samsung-written strokes keep
  their input byte for byte. `INK_INVENTORY_RECIPE` is `ink-inventory-v4:…:shape-skip:stylusless-inkpen2`.
  Tests: `tests/inkpen2_writer.rs` (synthetic note, real `prepare_append`, page render).

- **Page composite (intended, 2026-10-28).** Pages the renderer-only path refuses are no longer left without an
  image. `ink/prepare.rs` keeps the old admission byte for byte for renderer-only pages (`ink-reconstructed`, same
  bodies, same `:ink-reconstructed-v2` cache key) and builds a *composite* body (`{"composite":1,…}`,
  `ink/objects.rs`) for every other page of a page-based note: pictures (`fillEffect type=image imageId` = content
  file id → its `fileHash`, fetched through the account transport, SHA256-verified, cached in
  `page-previews/media/`), shapes and lines (ours and foreign, from the saved `path/segmentList`, frame-mapped
  `drawnRect`→`rect`), page text boxes (sent to the GPL CLI as `textBoxes`, laid out by sdocx with its pinned fonts,
  outlined), strokes the renderer does not admit (round polylines in the saved colour/width; eraser and replay-only
  records are not painted), empty sheets and the trailing list-compatibility page of a text flow, and a calm
  placeholder for unknown objects. Body formatting the renderer does not take is dropped (lenient flow, notice)
  instead of refusing the note; a text-only flow note is drawn too. Paint order: page colour, composite items in
  layer order, then the renderer layer (strokes, flow, text boxes) on top with its background stripped
  (`rs_render::compose`, resvg with `raster-images`). Manifest: `previewKind: "page-reconstructed"`, `complete`
  false with a calm `previewNotice` when something is approximated; cache key `:page-reconstructed-v1`. Pictures not
  downloaded yet (offline) give a stand-in `<key>.partial.png` with a placeholder, served at once while the full
  page is retried (at most every 10 min when connected). A renderer failure on a renderer-only page (sample range,
  output size, text layout) falls back to a forced composite stand-in.
  `INK_INVENTORY_RECIPE` is `ink-inventory-v5:…:page-composite` (inventory items may carry `kind`/`notice`).
- **Canvas and PDF stand-ins (intended, 2026-10-28).** A canvas-cache page whose cache cannot be downloaded or
  decoded (or any canvas page while offline) is drawn locally, scaled to the canvas size and cached as
  `<key>.local.png`; offline pages the local renderer cannot draw keep `preview_offline` 503. A PDF page with
  strokes but without Samsung's layer gets the page's own objects drawn over the Poppler image
  (`:pdf-local-ink-v1:` key, `previewKind: "pdf-with-notes"`); when Samsung's layer fails the local layer stands in.
  Signed picture links refused once are renewed by one read-only note GET per 10 min (as for PDFs).
  `MAX_DOWNLOAD` and the SPI input bound are 32 MiB (photo-heavy canvas caches and pictures exceed 8 MiB).
- **Free-floating objects (intended, 2026-10-28).** A note with a non-empty `floatingObjectList` (a picture inserted
  on a phone) was refused whole ("Bu not eklenmiş nesneler taşıyor."). Each floating object is now drawn by the page
  composite of the page its `pageIndex` names, after (over) that page's layer objects, from its own `rect` taken as
  page-local (observed on a phone note with `pageIndex` 0; other indices unobserved); a frame that misses the page,
  or an unplaceable `pageIndex`, is flagged (`NOTICE_HIDDEN`), never moved. Such a page always takes the composite
  path. In the lenient body reading an `objectSpan` anchor (the U+FFFC the body text places for that object) is
  dropped and its placeholder becomes a space (UTF-16 offsets kept). `INK_INVENTORY_RECIPE` gains `:floating-objects`.
  Tests: `tests/objects.rs` `a_free_floating_picture_is_drawn_on_its_page` (fixture `floating_note.json`, generated by
  notes-host `regen_preview_floating_fixture`).
- **Composites without the ink renderer (intended, 2026-10-28).** The local inventory was only read when
  `provider_available()` (renderer binary built + sandbox usable), so without the renderer (CI test jobs, a macOS
  build without `ink-render`) picture/shape/blank pages had no image although `render_composite` draws them
  in-process (resvg) and falls back to approximated polylines for ink. Now composite pages are listed regardless;
  renderer-only pages still need the renderer. Reproduced on Linux by pointing `LINUX_NOTES_PROJECT_ROOT` at a tree
  without `tools/ink-renderer/target` (the two `tests/objects.rs` failures seen in macOS CI); not run on macOS.
- **Page geometry for every page (intended, 2026-10-28).** Manifest pages without an image keep `width`/`height`
  from the page XML (the pen and the placeholder use them).

- **providerAvailable (intended, 2026-10-27).** With the built-in decoder the manifest reports
  `providerAvailable: true` for notes whose candidates include a canvas cache or that have no candidates, as the
  Python application did when its optional decoder was installed. The golden conformance fixtures were recorded
  without that decoder (`false`); see crates/notes-engine/PORTING.md.

## Verification

- **Tests:** `cargo test -p notes-preview` runs 50 tests (+3 ignored: fixture regeneration and long fuzz runs).
  - They port test_previews, test_previews_review, test_preview_scheduling, test_pdf_background (including
    the manager parts and the real Poppler render), test_thumbnails, test_ink_previews and the
    test_preview_performance snapshot checks.
  - They include golden `_prepare_pages` bodies from the pure-Python oracle (`tests/fixtures/ink_cases.json`,
    13 synthetic cases).
- **Differential:** 18 synthetic notes covering ink, flow text, dark pages, PDF with and without an overlay,
  invalid PDF placements, cached and missing canvases, and text.
  - **Manifest JSON:** byte-identical to pure Python for all 18 notes.
  - **Against `NOTES_IMPL=rust` Python:** every response and all 32 cache files are identical, including
    file names and PNG and thumbnail bytes. This holds both in-process and with self-exec.
  - **Against pure Python:** identical once `SAMSUNG_NOTES_INK_RASTER=rsvg` is set.
- **Canvas-cache decoder:** pixel-identical to the original codec (run only as a private local oracle) on 894
  natively encoded synthetic images, on the 97 constructed fixtures and on one real cached page, whose PNG is also
  byte-identical to the PNG the app had cached from the old decoder. See docs/CANVAS_CACHE.md.
