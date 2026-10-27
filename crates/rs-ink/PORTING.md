# rs-ink — Rust port of the ink area

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Python modules replaced (all public names, 32 total):

| Module | Rust exports |
|---|---|
| `samsung_notes.ink` | `InkError`, `CanvasPreviewCandidate`, `InkInventory`, `SpiHeader`, `unwrap_stroke_resource`, `inspect_ink_payload`, `inspect_spi` |
| `samsung_notes.ink_coordinates` | `stroke_owner_extent`, `page_coordinate_scale`, `rebase_style_bytes`, `override_style_bytes` |
| `samsung_notes.ink_cache` | `release_page_cache`, `pdf_background` |
| `samsung_notes.ink_editor` | `StrokeSamples`, `StrokeFrame`, `StrokeRecord`, `StrokeBinding`, `InkBinaryPart`, `InkAppendEdit`, `read_stroke_style`, `bind_stroke_metadata`, `decode_point_channels`, `encode_point_channels`, `decode_stroke_resource`, `replace_stroke_channels_offline`, `prepare_ink_clone` |
| `samsung_notes.ink_capture` | `NormalizedCapture`, `normalize_capture`, `basic_pen_drawn_bounds`, `prepare_ink_append`, `prepare_ink_append_v2`, `prepare_ink_append_v3` |

Private Python helpers (`_f32`, `_prepare_ink_document`, `_INVENTORY_MEMO`, `_INVENTORY_LOCK`,
`STYLE_FIELDS`, `WRAPPER`, constants) remain Python. Other areas still import them
(`ink_erase` uses `_INVENTORY_MEMO`; `ink_render`/`ink_projection`/`ink_first_stroke` use `_f32`).
Once those areas are ported, they should be replaced with the Rust core equivalents
(`core::num::f32r`, `core::binary::style_field`, `core::binary::WRAPPER`).

## Layout

- `src/core/` holds the pure-Rust code, with no PyO3. A Tauri crate can link it directly.
  - `binary.rs`: frame chain, Q5/Q12 channel decode/encode, wrapper/SPI headers, style fields.
  - `coords.rs`: stored owner extent, f32 page scale, rebase of fields 3/8/13, recipe-3 override.
  - `capture.rs`: `normalize_capture` quantization (sm_ReduceStroke) and basic-pen bounds.
  - `xml.rs`: a small DOM following ElementTree's text/tail model. It parses with roxmltree and
    serializes byte for byte like `ET.tostring(e, encoding='utf-8', xml_declaration=True)`:
    attribute escaping including `&#10;`/`&#13;`/`&#09;`, ` />` empty elements, the element tail,
    and the `ns0`/well-known prefix assignment.
  - `cache.rs`: `release_page_cache` and `pdf_background`. It can log every mutation as `Op::Set` / `Op::Remove`.
    PDF background entries (one pdfData per page, pdfRect = page box, refCount = references)
    are validated and never changed; only canvas caches are released (2026-10-26, PDF pages).
  - `prepare.rs`: `bind_stroke_metadata` and `_prepare_ink_document` (recipes 1/2/3). It keeps a
    Rust-side 4-entry inventory memo keyed like the Python one. Payload parsing is injected as a
    closure.
  - `payload.rs`: `inspect_ink_payload` over the parsed-payload fields it needs.
  - `num.rs`: Python numeric semantics:
    - `_f32` rounding;
    - `Val`, which keeps the exact-type facts behind `type(v) in (int, float)`;
    - `repr(float)`, the shortest correctly rounded digits with Python's exponent rules;
    - `float(str)`.
- `src/pyglue.rs` converts between Python objects and the core, and maps errors to the Python
  exception classes.
- `src/<module>.rs` holds thin `#[pyfunction]` wrappers.

## Public API for other crates

See `STABLE`:
- `core::prepare::prepare_append` / `append_v2` / `append_v3`, with `AppendSnapshot` (or
  `AppendSnapshot::from_doc`) and `RawSamples::from_parts`, are pure Rust.
- `pyapi::ink_error` and `pyapi::append_edit_to_py` build Python objects from the live classes.
- The lib test `api_tests` compares the public entry against Python-dumped fixtures; set
  `INK_API_CASES` to the fixture file to run it.

## Behaviour notes

- **Dataclasses.** Built at import with `dataclasses.make_dataclass`, using the same frozen, eq,
  repr and per-field `repr=False` settings and defaults, and `__module__` set to the original
  module. As a result, `dataclasses.replace`, equality, hashing and `isinstance` work the same.
  These are plain data containers with no logic.
- **`InkError`.** Created in Rust as `samsung_notes.ink.InkError(ValueError)`. Every module imports
  it after the override, so all of them share the class.
- **Other exceptions.** `DocumentError`, `PayloadError`, `struct.error` and `ET.ParseError` are
  looked up from their Python modules when raised. Messages are verbatim, including the Turkish
  `document.one` / `document.integer` strings.
- **`release_page_cache` with Python ElementTree input.**
  1. The tree is mirrored into the Rust DOM.
  2. The core runs.
  3. The logged mutations are replayed onto the Python elements with `.set` / `.remove`.
  4. If the core failed, the error is raised afterwards.

  Replay also runs before an error is raised, so a partially mutated tree on error matches Python.
- **Equality checks.** `encode_point_channels` and `replace_stroke_channels_offline` do their
  final round-trip checks with Python `==` on the dataclasses. This keeps Python's tuple/list
  equality semantics for caller-supplied samples. The pure-core versions use Rust equality.
- **Check order.** The order of checks follows the oracle, so the first error raised is the same.
  For example, `now_ms` type is checked before `syncModifiedTime` is parsed, and in recipe 3
  `penSize` is checked before the override.
- **Known deviations, pathological inputs only.**
  - Ints wider than i128.
  - Non-str attribute values set programmatically on ET elements: these are mirrored as `str(v)`.
  - Non-UTF-8 encodings declared in `resource_xml`.
  - XML comments inside text.

## Remaining PYFALLBACKs: 0

All three earlier fallbacks now go through the rs-doc rlib (`crates/rs-doc/STABLE`; the
dependency is renamed `rs_doc` because the rs-doc library is named `_doc`):

- **MIME parsing** uses `rs_doc::payload::parse_payload`, with the live `tools.notes_payload`
  limits read through `rs_doc::pyapi::payload_limits`:
  - `core::prepare::payload_parts`, used by the append preparation;
  - `core::payload::inspect_raw`, used by `inspect_ink_payload` for bytes + str input.
  For unusual argument types, `inspect_ink_payload` calls `tools.notes_payload.parse_payload`
  through Python so the oracle's error is raised.
- **`PayloadResult` and `DocumentSnapshot` isinstance checks, and `DocumentSnapshot.from_dict`**,
  go through the Python-visible `tools.notes_payload` / `samsung_notes.document` objects. This
  follows rs-doc's rule that other cdylibs must not construct its pyclasses.
- **`DocumentError` / `PayloadError`** are raised via `rs_doc::pyapi::document_error` /
  `payload_error`.

## Verification

- Ink strict (ink modules under rust-strict, others under rust):
  - the 8 ink test modules: 47 tests pass;
  - all 20 ink-related test modules, including cloud_ink*, ink_application*, projection, erase,
    first_stroke, render, previews and drafts: 138 tests pass.
- **Global `NOTES_IMPL=rust-strict`.** The 7 core ink test modules pass (40 tests).
  `test_ink_style_v3`, `test_cloud_ink*` and `test_ink_application*` import `engine` →
  `session.SessionManager` (auth area, ported last), so they cannot import yet. They pass with
  strict scoped to the ink and doc modules (148 tests across 22 modules).
- **Differential check (Python oracle vs Rust), 0 differences:**
  - 3000 random captures: `normalize_capture`, bounds, encode/decode in both compressed modes,
    and random `decode_stroke_resource` input;
  - 13 document cases: clone, v1, v2, v2-legacy, v2-nocache, v3, error paths and dict snapshot;
  - 150 random v2/v3 documents.

  Compared values are `full_xml`, `tree_xml`, `upload_xml`, stroke binaries and the exception
  type and message.

## Perf notes

The codec and document preparation run natively. The inventory memo stores decoded Rust records,
so repeat batch stroke preparations skip both MIME parsing and binary decoding. The
Python→Rust StrokeRecord conversion copies only the frames that are needed (`record_frames`).

## PDF/template pages (2026-10-26)

- `release_page_cache` accepts pages with `pdfDataList`; the PDF content file must be referenced
  exactly `refCount` times, must not double as a cache binding and stays byte-identical
  (messages `PDF reference count differs`, `PDF background file missing`,
  `unsupported PDF background metadata`, same order as Python).
- `prepare_ink_document`: recipe 1 keeps the 32-page bound; recipes 2/3 use
  `MAX_DOCUMENT_PAGES = 2048` (PDF books).
- Evidence: `artifacts/analysis/ink-pdf-pages-v1/findings.json`.
