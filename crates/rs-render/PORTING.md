# rs-render: Rust port of the render area

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

All public names are in Rust: ink_render 6/6, ink_render_job 1/1, ink_render_worker 1/1,
previews 5/5, page_image 2/2, thumbnails 3/3, pdf_background 9/9, pdf_background_worker 1/1,
preview_codec 3/3, preview_contract (constants), preview_worker 2/2. Private helpers that the
tests import are exported too: `_png_valid`, `_rgba`, `_Entry`, `_rgba_png`, `_prepare_pages`,
`_PageInput`, `_url`, `_codec`, `_payload_from_snapshot`, `_inventory_from_snapshot`,
`_read_ppm`, `_valid_cached_pdf`, `_job`, `_run`, `_snapshot_bytes`, and the flow/body helpers.

Oracle: the render tests (60) pass under `NOTES_IMPL=rust`, and so does the full suite (388 tests
plus 79 tools tests). previews and thumbnails get no STRICT mark on the scoreboard. The reason is
that their import chain reaches `capture_oauth_callback`, which is in the auth area and not ported
yet. All of their own names are ported.

## Layout

- **Pure Rust, no PyO3.** A Tauri crate can link these directly.
  - `pixels.rs` handles the RGBA8/filter-0 PNG contract:
    - encode, using the system zlib at level 6. Its bytes are identical to CPython's
      `zlib.compressobj(6)`, which was verified.
    - validate, with CRC checks.
    - decode.
    - `unfilter_png`, which handles all five filters for RGB/RGBA.
    - box downsample, HSL light-from-dark, PDF compose and PPM placement.
  - `pdf_layout.rs` builds the PDF placement inventory with roxmltree over already-validated XML.
  - `sandbox.rs` covers rlimits, PDEATHSIG parent binding, CPU pinning, the bwrap argument lists,
    and spawn with timeout, setsid and session kill.
  - `raster.rs` is resvg.
- **ink_render.** The stroke decode, metadata binding, admission and renderer JSON are native,
  using the `rs_ink::core` rlib. The JSON is written in the exact `json.dumps(separators=(',',':'))`
  form, with `repr(float)` digits.
- **PyO3 wrappers and orchestration.** `PreviewManager` and `ThumbnailManager` are
  `#[pyclass(subclass, dict)]`. Their queue, threads, locks and jobs stay Python objects, because
  callers and tests drive them through those objects (`manager.queue.join()`, `manager.jobs`,
  instance-patched `_entry` / `_source`).

## Behaviour notes and deliberate deviations

1. **Byte-exact outputs.**
   - PNG bytes are identical to the Python encoder.
   - Renderer input bodies are identical to Python `_prepare_pages`. This was checked by comparing
     SHA256 for 6 fixtures: Fountain 14/18, the SDK empty body, body-text flow, width-owner scaling,
     and a synthetic 300-stroke note.
2. **Worker processes.** The app no longer starts `python -m samsung_notes.{pdf_background,ink_render}_worker`.
   - pdftoppm, the renderer and rsvg-convert are exec'd through the same bwrap argument lists.
   - The limits are applied in the forked child: rlimits, PDEATHSIG with a ppid check, the lowest
     CPU, and setsid for ink.
   - The worker `main()` functions are still ported, for compatibility.
3. **Ink rasterizer.** It defaults to in-process resvg/tiny-skia. Set `SAMSUNG_NOTES_INK_RASTER=rsvg`
   to get the sandboxed rsvg-convert path back.
   - Pixel difference against rsvg-convert:
     - strokes: max 15/255, mean 0.024;
     - text edges: max 47 on 23 pixels.
   - The SVG it reads is our own renderer's output, bounded to 16 MB with `<image>` and `<script>`
     rejected.
   - rsvg-convert stays in `provider_available()`, as in Python.
   - **This step is no longer process-isolated or rlimited. Revert it if that is not acceptable.**
4. **Preparation child.** `render_ink_page` / `render_ink_svg` start one streaming child per note
   (`python -E -c` loading this extension's `ink_render_job._serve`). The child writes, in order:
   1. the inventory;
   2. the requested page body;
   3. every other page.

   Further details:
   - The parent caches the bodies (96 MB, 4 notes), so other pages of the same note skip
     preparation entirely.
   - Inside the child, the verified resource and the decoded strokes are cached per exact source,
     so the inventory and all pages share one decode.
   - Error messages and their mapping are the same as Python: no admitted page gives "bulunmuyor"
     (UI text: "not found"), a failed body gives "hazırlanamadı" ("could not be prepared"), and a
     timeout gives "süresi doldu" ("timed out").
   - `inspect_render_pages` still uses the `-m samsung_notes.ink_render_job inventory` child,
     which runs the Rust `_prepare_pages`.
5. **Renderer not linked in process.** The GPL sdocx renderer is not linked into the extension.
   - It costs 0.3 s per page inside bwrap, so the gain would be small.
   - Linking it would remove its CPU and memory limits.
   - Linking it would also make this extension GPL-3.0.
6. **HTTP stays on Python's standard library.** The HTTP transports (`previews._http`,
   `_NoRedirect`, and fetch_pdf's default download) use the stdlib `urllib` / `ssl` through PyO3.
   They cannot be tested here without network.
7. **Path length.** `math.hypot` versus Rust `f64::hypot` can differ in the last ulp. This only
   matters for a page whose path length lands exactly at `MAX_DISTANCE` (500000).
8. **Exception classes.** These are pyclasses: `PreviewError(Exception)`, `PdfBackgroundError(ValueError)`,
   `PreviewCodecError(ValueError)` and `InkRenderError(ValueError)`. The first three set
   `args`/`message`/`code` like the Python `__init__`. `RenderError` is the same class as
   `InkRenderError`.
9. **Dataclasses.** `PdfBackground`, `InkGeometryPreview` and `_Entry` are real dataclasses, built
   with `dataclasses.make_dataclass` in `src/dataclasses.rs`. They keep the same frozen setting,
   `repr=False` fields and defaults. `PdfBackground.complete` and `is_dark_mode` are properties
   backed by Rust functions. As a result, `dataclasses.replace`, eq, hash and repr behave exactly
   like Python.
10. **Expired PDF links.**
    - `PdfLinkExpired(PdfBackgroundError)` is raised by `fetch_pdf` on HTTP 401 or 403.
    - `PreviewManager._fetch_pdf_refreshing` does one read-only `get_note`, then
      `store.apply_document`, then re-inspects through the module-level `_payload_from_snapshot` /
      `inspect_pdf_backgrounds`, and retries once.
    - It is exposed through `functools.partialmethod`, so the oracle's unbound call with a duck-typed
      `self` works.

## Cross-area access (src/xarea.rs)

- **Class and constant lookups** go through the owner module: StoreError, DocumentError, InkError,
  PayloadError, PayloadResult, APIError, DocumentSnapshot, CanvasPreviewCandidate, InkInventory,
  MAX_*, SPAN_TYPES and ALIGNMENTS. Class identity lives there, so these are not counted as
  fallbacks.
- **Native rlibs:**
  - **rs-ink core:** stroke decode, bind and scale; `inspect_ink_payload` over the PayloadResult
    fields; `inspect_spi`; `f32r` / `py_repr`.
  - **rs-ops:** `ink_projection::verify` (verified_resource). The admission walk runs on
    `ET.fromstring(snapshot.resource_xml)`, which is byte-identical to the verified root. The
    stroke parts come from the verified payload, so there is no second MIME parse.
  - **rs-editor:** `edit::object_rich` (the Python ET side-car lists are mirrored first), plus
    `rich::{decode, b64, utf16_length}`.
  - **rs-doc:** `doc::integer`.
  - **rs-store:** `storage::private_directory`.
  - `document.one` is a five-line local helper over ElementTree.

### Remaining PYFALLBACKs (2)

| Call | Why it remains |
|---|---|
| `tools.notes_payload.parse_payload` (previews `_payload_from_snapshot`, pdf_background on raw input) | The oracle patches `tools.notes_payload._parts` and asserts exactly one MIME parse per manifest. Only the module-level function honours that patch, so the rs-doc core cannot be called directly. Under `NOTES_IMPL=rust` this already runs rs-doc's Rust `parse_payload`. |
| `samsung_notes._maetel_native.NativeOracle` | Retired 2026-10-27: the emulator worker entry (`preview_worker.main`), its `xarea` import, the pinned native-library constants and the provider check were removed; `preview_codec` (feature `python`) now reports the provider absent. Historical glue: `reference/spi-emulator-glue/`. The product decoder is notes-preview `spi`. |

## Performance

Synthetic data, 16-core machine shared with other builds (load 2.5–5). Times are medians or best
of 2 runs.

| Step | Python | Rust | Speed-up |
|---|---|---|---|
| ink inventory, 3000 strokes, cold | 11.5–12.1 s | 0.55–0.57 s | ≈21× |
| ink inventory, 1000 strokes, cold | 3.8–4.0 s | 0.32–0.33 s | ≈12× |
| `render_ink_page`, 250-stroke Fountain page, cold (app start) | 5.6–9.0 s | 1.47–1.50 s | ≈4–6× |
| `render_ink_page`, same page again | 4.5–4.6 s | 1.09–1.22 s | ≈4× |
| filtered PNG canonicalisation (`_rgba_png`, 1440×2048) | 2.8 s | 0.62 s | ≈4.5× |
| light_from_dark | 0.39–0.43 s | 0.09–0.14 s | |
| downsample_page | 0.26 s | 0.04 s | |
| compose_pdf_canvas | 0.32–0.46 s | 0.15 s | |
| PNG encode and decode | ~60 / 20–55 ms | ~45–65 / 13–50 ms | zlib-bound |

Notes on the table:

- **What the inventory figure includes.** Part of the inventory gain comes from the doc and ops
  areas now being Rust too: MIME parsing and resource verification.
- **Where the 250-stroke render time goes in Rust.** Renderer 0.33 s, resvg 0.54 s, PNG encode
  0.1 s, plus preparation in the child on a cold start.
- **With rsvg-convert instead of resvg.** The rasterizer alone is 2.9 s, and a cold render is
  about 2.6 s in total.
- **Synthetic data only.** InkPen2 renderer input was checked on synthetic strokes only: for
  `8;`/`9;`, tool 2 and several property masks, admission, rejection and the
  `inkpen2Profile`/`admit` bodies are byte-identical to Python. A real InkPen2 note has not been
  measured.

## Highlighter / Marker4 (2026-10-27)

`_admitted_pen` admits Marker4 (`7;`/`8;`, tool 0/2/3, binary_added, the listed property/style
gates, penSize 0..800, 2..65536 points); a page body gets `"admit":"calibrated"` when it holds a
Marker4 stroke and no InkPen2 stroke. Mirrored in `crates/notes-preview/src/ink/prepare.rs` for
the Python-free engine.

## Shape/line objects on ink pages (2026-10-27, intended divergence from Python)

Rust is canonical here; `samsung_notes/ink_render.py` was deliberately left unchanged and still
refuses every page that holds a non-stroke object. Under `NOTES_IMPL=rust` (and in the pure engine,
`crates/notes-preview/src/ink/prepare.rs`) page admission now:

- leaves out of the renderer input every object `rs_ops::page_shapes::renderer_skippable_shape`
  accepts: `read_shape_object`'s own profile plus an exact attribute-name allowlist on the object
  and every descendant (only `appendTime`/`ownerPageWidth`/`ownerPageHeight` may be absent), path
  segments of type 1/2/3/6 with exactly their coordinates, no element text. The UI draws these
  shapes as SVG geometry (desktop/src/ShapeTool.tsx). Any other object (unknown type, text box,
  image, device-edited shape with extra attributes) keeps the page refused as before;
- admits a page whose only objects are skipped shapes as a plain background: its body is exactly
  `{"schemaVersion":1,"width":W,"height":H,"backgroundArgb":C,"strokes":[]}` and
  `render_ink_page`/`render_ink_svg` draw it without the GPL renderer (`background_only`,
  `background_pixels`, `background_svg`; solid page colour, same PNG encoder);
- stroke bodies of shape pages are byte-identical to the same page without its shapes.

`previews.INK_INVENTORY_RECIPE` is `ink-inventory-v3:…:shape-skip` so stored "refused" inventories
are recomputed. Tests: `tests/test_ink_render_shapes.py` (runs only under `NOTES_IMPL=rust*`),
`crates/notes-preview/tests/ink.rs` (fixture `tests/fixtures/ink_shape_cases.json`, regenerated by
`reference/python/generators/notes-preview/gen_ink_shape_cases.py`). Shapes are not drawn into the PNG; no native pixel equality is claimed.
