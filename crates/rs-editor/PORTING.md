# rs-editor — port of `samsung_notes/editor_document.py`

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

## Status
- All 14 public names are Rust: `RichText`, `utf16_length`, `normalize_model`,
  `read_rich_text`, `encode_rich_text`, `remap_sections`, `editor_model_fingerprint`,
  `title_content_fingerprint`, `editor_content_fingerprint`, `prepare_title_edit`,
  `editor_projection`, `new_editor_document`, `prepare_editor_edit`, `prepare_rich_edit`
  (the same function object as `prepare_editor_edit`, as in Python).
- Oracle tests (8 modules, 70 tests) pass under `NOTES_IMPL=rust`; full `tests` (386) and
  `tools` (79) suites pass under `NOTES_IMPL=rust`. Global `rust-strict` import currently
  fails only because `tools.notes_payload` / `samsung_notes.document` (area doc) are not yet
  ported. Under global `NOTES_IMPL=rust-strict`, the editor_document scoreboard shows STRICT. 3 oracle modules (42 tests) pass; the other 5 fail at import only because `samsung_notes.session` (area auth) is not ported. Those 5 (33 tests) pass under `rust`.
- Differential fuzz against the Python implementation (random streams, models, sections,
  namespaced/indented/CR XML, title/body edits, projections): 0 mismatches over ~25k calls,
  incl. bytes-exact `full_xml`/`tree_xml`/`upload_xml`, exception class and message.

## Layout (pure Rust core, thin PyO3 wrappers)
- `pyval.rs`: Python-like value tree (`Val`, dict equality unordered, bool/int/float
  cross-equality), `int()`/`float()` string parsing, `float.__repr__`, `str.isspace`,
  strict `b64decode(validate=True)` (binascii strict-mode algorithm), `json.dumps`
  (indent / sort_keys / compact, ensure_ascii).
- `etree.rs`: ElementTree-compatible tree: parse (roxmltree), `ET.tostring` byte-exact
  (`ns0..` prefixes, well-known prefixes, escaping, `<x />`, tails), and
  `ET.canonicalize(ET.tostring(..))` simulated directly (C14N 2.0 defaults, lazy ns
  declarations, CR normalization of re-parsed text).
- `rich.rs`: codec — span/paragraph/section wire grammar, normalize_model, read/encode,
  remap_sections, model fingerprint. `Error` maps to DocumentError / ValueError / TypeError /
  OverflowError / AttributeError / ET.ParseError or re-raises an external PyErr unchanged.
- `edit.rs`: title/body edit preparation, projection, fingerprints. Dependencies on
  area `doc` go through the `Backend` trait / closures.
- `editor_document.rs`: PyO3 wrappers, `RichText` pyclass (frozen, subclassable, dataclass
  eq/hash semantics, `to_dict`, `__match_args__`).

## Dependencies on area doc (PYFALLBACK: 0)
rs-doc rlib (`rs_doc = { package = "rs-doc" }`): `writer::object_hash`, `doc::plain_structure`
(on `xml::fromstring(xml_bytes(temporary))`), `mime::parts` (+ live limits via
`pyapi::doc_limits`), `doc::new_document`, `pyapi::document_error/payload_error`; these sit
behind `edit::DocBackend`. Python-visible objects go through `samsung_notes.document`
(`DocumentSnapshot` isinstance/`from_dict`, `DocumentEdit`, and `new_document` only for
unusual argument types), as the doc owner asked. No rs-doc pyclass or static exception is built here.
Still local (verified equivalent by differential fuzz): `etree.rs` (tree/tostring/C14N), plus
`text_value`, `encode_text`, `integer`, `one`. They can be swapped for `rs_doc::xml` / `rs_doc::doc`
later. I kept them for now so the fuzz-verified byte-exact path does not change.

## Behaviour notes / known divergences (exotic inputs only)
- Python `\d` / `int()` / `float()` accept non-ASCII Unicode digits; Rust accepts ASCII only.
- `remap_sections` with non-int start/end raises DocumentError (Python: TypeError or
  DocumentError depending on type).
- Strings with lone surrogates in hyperText data are rejected as invalid instead of passing.
- XML parse: UTF-8 (+BOM) and declared latin-1 only; parse-error messages are roxmltree's
  (class is still `xml.etree.ElementTree.ParseError`).
- Model values nested deeper than 8 containers are treated as opaque objects.
- `RichText` lacks `__dataclass_fields__` (no `dataclasses.replace/asdict`); not used.

## Perf
Codec and XML work no longer touch Python objects; fallbacks remain for the doc-area calls
listed above (one `_plain_structure` re-parse per projection).
