# rs-ops — Rust port of the offline document operations

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Python modules replaced (all public names, 31 total):

| Module | Rust exports |
|---|---|
| `samsung_notes.ink_projection` | `project_ink`, `verified_resource`, `project_page_geometry` |
| `samsung_notes.ink_erase` | `InkStrokeEdit`, `stroke_erasable`, `prepare_ink_delete`, `prepare_ink_move`, `expected_stroke_parts`, `check_compare_response` |
| `samsung_notes.ink_first_stroke` | `first_stroke_style`, `encode_first_stroke`, `prepare_first_stroke` |
| `samsung_notes.ink_highlighter` | `is_highlighter`, `highlighter_style`, `highlighter_drawn_bounds`, `encode_highlighter_stroke`, `prepare_highlighter_stroke` |
| `samsung_notes.page_style` | `PageStyleEdit`, `prepare_page_background`, `page_background_state` |
| `samsung_notes.page_template` | `PdfPagesEdit`, `pdf_page_sizes`, `prepare_pdf_pages` |
| `samsung_notes.page_ops` | `PageOpsError`, `PageOpsEdit`, `prepare_add_page`, `prepare_delete_page`, `prepare_reorder_pages`, `page_ops_state`, `note_height`, `build_blank_page` |
| `samsung_notes.page_shapes` | `ShapeError`, `ShapeEdit`, `quantize`, `jfloat`, `signed`, `normalize_shape`, `shape_path`, `build_shape_object`, `read_shape_object`, `prepare_add_shape`, `project_shapes` |

Module constants (`PROJECTION_RECIPE`, `PAGE_PALETTE`, `MAX_*`, `PEN_NAME`, …) stay Python
attributes; the Rust core carries identical copies (re-check them when the Python side changes).

## Layout

- Pure-Rust core (no PyO3 types in signatures):
  - `xml.rs`: mutable ElementTree-model DOM (`text`/`tail`, comments dropped) parsed with
    roxmltree. `xml_bytes` is byte-exact with `ET.tostring(root, encoding='utf-8', xml_declaration=True)`,
    including attribute escaping, ` />`, element tails and `ns0` prefixes.
  - `err.rs`: `Error { kind, msg }`. `kind` mirrors the Python class hierarchy and `Error::is`
    gives isinstance semantics: Ink/Doc/PageOps/Payload/Unicode are ValueError, PageOps is Doc.
    Every `except (…)` in the oracle is a `kinds` test.
  - `util.rs`: `document.integer`/`one`/`normalize_commit`, `notes_writer.parent_hash`/`object_hash`,
    `bytes.fromhex`, `_f32`, `repr(float)`, uuid4.
  - `deps.rs`: the `Deps` trait the operations need from the doc/ink areas, plus the shared data types.
  - `<module>.rs`: the core functions (`project_ink`, `prepare_ink_delete`, `prepare_pdf_pages`, …),
    each returning plain structs (`StrokeEdit`, `PageOps`, `PdfPages`, …).
- `bridge.rs`: the PyO3 boundary.
  - Exception mapping in both directions; foreign Python errors are re-raised unchanged.
  - ElementTree ↔ DOM conversion.
  - `PyDeps`, which implements `Deps`:
    - ink functions call the **rs-ink core**: decode, bind, coordinates, style, channels,
      `release_page_cache`, `normalize_capture`, drawn bounds, and the `prepare_ink_append_v2`
      dry run.
    - MIME parsing calls the **rs-doc core**: `payload::parse_payload` with the module's
      (patchable) limits.
- `record.rs`: macro for frozen dataclass-like pyclasses. It provides a positional/keyword
  constructor, read-only fields, dataclass `__eq__`/`__hash__`/`__repr__`, and is subclassable.
  `InkStrokeEdit` keeps its PRIVATE repr and its `content_file_ids=()` / `recipe_version=1` defaults.

A Tauri/pure-Rust consumer implements `Deps` directly on rs-doc/rs-ink, with no Python: only
`snapshot`/`convert`/`verified`/`resource_xml` touch Python objects in `PyDeps`.

## Remaining PYFALLBACKs: 0

The Python `PayloadResult` that `verified_resource` returns is built by
`rs_doc::pyapi::payload_result_to_python` from the rs-doc core parse.

## Public Rust API for other crates (add-only)

- `page_shapes::renderer_skippable_shape(&Element) -> bool`. Stricter than `read_shape_object`
  (exact attribute names on the object and every descendant); rs-render and notes-preview skip
  such objects when building ink renderer input. No Python counterpart.
- `page_ops::load(deps, snapshot, now_ms) -> R<Loaded>`. This is `page_ops._load`. `Loaded` holds
  `root`, the `ai`/`ti`/`pi` section indices, `tick`, and `pages()`. rs-store uses it.

### Boundary class lookups (not counted as fallbacks; no logic runs in Python)

These are lookups of the same kind as exception classes:

- `samsung_notes.document.DocumentSnapshot`: `isinstance` and `from_dict`. The class is rs-doc's.
  `page_style` and `page_template` resolve `<module>.DocumentSnapshot` at call time, like the oracle.
- `samsung_notes.ink_editor.{InkBinaryPart, InkAppendEdit, StrokeSamples, StrokeFrame, StrokeRecord}`
  and `samsung_notes.ink_capture.NormalizedCapture`: output and `isinstance` types, built by rs-ink
  as dataclasses.
- Exception classes: `InkError`, `DocumentError`, `PayloadError`, `ET.ParseError`, and the
  Rust-created `PageOpsError(DocumentError)`.

## Behaviour notes

- **Check order.** It follows the oracle line by line. For example, `now_ms` type is checked before
  `syncModifiedTime` is parsed where Python short-circuits. A TypeError from iterating `stroke_ids`
  or `page_ids` is raised at the Python point of use, after `_load`.
- **`verified_resource` is resolved at call time.** `page_style`, `page_template` and `page_ops`
  look up `samsung_notes.ink_projection.verified_resource` when they run. If a test patches it
  (`tests/test_cloud_pdf_pages.py::PdfTreeOrderTests`), the patched callable is used and the
  snapshot is read duck-typed (`resource_xml` only), as in Python. Otherwise the Rust core runs
  directly.
- **`page_template`.** A new noteTree `contentFileList` is inserted right after the tree
  `pageList`, matching the server order pageList, contentFileList, bodyText seen in live readback.
  `pdf_page_sizes` still shells out to `pdfinfo -f 1 -l 201 <path>`. The GIL is released and the
  timeout kills the child. `OSError`, timeout, a non-zero exit status or no sizes →
  `DocumentError('PDF okunamadı.')`.
- **`ink_erase`.** The Python `_INVENTORY_MEMO` is no longer used, because decoding runs in Rust
  (rs-ink keeps its own memo for appends). Results are unchanged; there is no cross-module cache
  coupling.
- **`project_ink`.** The append probe is rs-ink's `prepare_ink_document` (recipe 2, a discarded
  centre sample). The erase probe is the Rust `prepare_ink_delete`.
- **Known deviations, pathological inputs only.**
  - Integers wider than i128.
  - Exact Python messages of some internal ValueErrors (uuid parsing, `fromhex`) that the oracle
    never surfaces.
  - Non-UTF-8 XML encodings.

## Verification

- `NOTES_IMPL=rust-strict` import: all 6 modules report STRICT in `tools/rust_conformance.py`.
- The 11 ops test modules pass under `rust` and under `rust-strict` (81 tests).
- Full suites under `NOTES_IMPL=rust` are green.
- Differential harness: Python and Rust run on the same synthetic fixtures, with only the ops area
  switched. All 196 cases match exactly (bytes hashed, dict DTOs, frozensets, ElementTree outputs,
  exception class and message), including 83 error cases. The harness covers every public function
  and the snap/cached/three-page/plain/stroke/cache-page fixtures.

## Performance (synthetic fixtures, mean of 50 runs, ops area Rust vs Python)

| Function | Python | Rust |
|---|---|---|
| `project_ink` | 8.8 ms | 1.9 ms |
| `page_ops_state` | 3.1 ms | 0.45 ms |
| `page_background_state` | 5.9 ms | 0.9 ms |
| `prepare_ink_delete` (Python memo warm) | 1.2 ms | 0.8 ms |

These are small fixtures; large real notes were not measured, per the privacy rules.

## PDF/template pages (2026-10-26)

- `ink_projection`: PDF notes are projected (recipe `ink-projection-v5:pdf-pages`). Pages carry
  `background` (`"pdf"` or None) and `ink` = {appendInk, eraseInk, firstStroke, reason}; the
  capabilities gain `firstStroke`. Stroke-free notes list their pages. Whole-note Turkish reasons
  for images, voice/attachments, groups, shapes/text boxes and floating objects
  (`note_ink_reason`, Python `_note_ink_reason`); per-page reasons (`page_ink_reason`). The
  first-stroke dry run runs once per (PDF, cache) page category through the new
  `Deps::first_stroke_probe`. Library copy: Python and Rust projections are equal on all 32
  notes (hash of the JSON DTO), Rust 3.4 s vs Python 31.4 s for the whole library.
- `ink_erase`: page bound `MAX_DOCUMENT_PAGES` (rs-ink core).
- `ink_first_stroke`: a PDF target page always goes through `release_page_cache` (file inventory
  validated; its cache, if any, released).

## Highlighter / Marker4 (2026-10-27)

- `ink_highlighter` (recipe 5): pure-Rust `highlighter_style`, `highlighter_drawn_bounds`,
  `encode_highlighter_stroke`, `prepare_highlighter_stroke` (same signature and `FirstStroke` return
  as `prepare_first_stroke`; the PyO3 wrapper sets `recipe_version=HIGHLIGHTER_RECIPE`) and
  `is_highlighter`. Shared with `ink_first_stroke` (now `pub(crate)`): `encode_stroke` with an
  `EncodeProfile` (property word, message prefix, extra property re-check), `ensure_strings(attrs,
  [pen, setting])`, `string_ids`, `frame`, style helpers. `prepare_first_stroke` output unchanged.
  Python `min`/`max` tie semantics kept for the drawnRect union; `bytes.fromhex` → `integer()` →
  32-byte check order kept for existing child hashes.
- `ink_projection` (`ink-projection-v6:highlighter`): Marker4 top-layer strokes listed as kind
  `highlighter` (colorARGB, size, points); per-page highlighter shape, one discarded dry run per
  (hasStrokes, pdf, cache) category through the new `Deps::highlighter_probe` (PyDeps and
  notes-host PureDeps); page `ink.highlighter` and capability `highlighter`. notes-host
  `areas.rs` emits the same keys.
- Differential (synthetic fixtures, ops area Python vs Rust, same pickled snapshots): 97 cases
  (30 successful edits/projections hashed byte-for-byte, the rest exception class + message)
  identical.

## page_shapes (Shapes / Şekil)

- Core: `page_shapes::{normalize_shape, prepare_add_shape, prepare_core, project_shapes,
  read_shape_object, build_shape_object, shape_path}` over plain inputs (`StyleIn`/`Fill`
  classify the caller's style the way the Python checks read it). `ShapeError` is a new
  `err::Kind::Shape` (DocumentError subclass; `is(Doc)`/`is(Value)` true), raised in Python as
  the class `samsung_notes.page_shapes.ShapeError` created in `bridge::shape_error`.
- Byte-exact with the oracle (differential: 91 successful edits + 309 refusals + projections,
  identical bytes/messages). `shape_path` returns floats where Python may keep the caller's
  ints (equal under `==`).
- Shared by rs-store (`shapes::dry_run`, `save_page_op` op `'shape'`) and the pure engine
  (notes-host `areas::project_shapes`, notes-cloud `ops::add_shape`).
