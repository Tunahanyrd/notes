# rs-cloud — Rust port of the Cloud transaction layer

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Modules: `samsung_notes.cloud_api` (4/4), `cloud_metadata` (4/4),
`cloud_ink_erase` (4/4), `cloud_page_ops` (7/7), `cloud_shapes` (3/3) — all public names in Rust.
Oracle: tests/test_cloud_*.py, test_page_ops_cloud, test_ink_erase_cloud,
test_ink_application*, test_editor_application, test_organization_store,
test_ink_style_v3, test_engine_review (157 tests) pass with rust-strict applied
to the four cloud modules; full suites green under NOTES_IMPL=rust (386 + 79).

## Layout
- `pyjson.rs` — pure JSON with Python `json` semantics: byte-identical
  `json.dumps` (indent=2 / compact / default, ensure_ascii, float repr) and a
  `json.loads` value model. Needed because `_json_once`/`_save_once` compare
  new bytes with receipts possibly written by the Python engine.
- `util.rs` — private_write/load_json/save_json/_save_once semantics,
  `gzip.compress(mtime=0)` (system zlib via flate2, CPython header bytes),
  uuid4/uuid5, secrets, urllib quoting, `notes_cloud.endpoint`, urlsplit subset.
- `etree.rs` — mutable ElementTree model + byte-identical `ET.tostring`
  (unicode / utf-8+declaration / us-ascii charrefs, ns0 prefixes, tails).
- `guards.rs` — all compareForUpsync guards, default_view, createUploadURLs scan.
- `glue.rs` — APIError/ConflictError/UnsupportedDocumentError as Rust
  exception classes (`extends=PyException`, `.code/.message/.ambiguous`, same
  messages), Python `except (...)` classification, file glue.
- `cloud_*.rs` — orchestration. CloudAPI/CloudMetadata are
  `#[pyclass(subclass, dict)]`; state lives in the instance dict and every
  internal call goes through Python attribute lookup, so subclasses
  (PageAPI), patched instance attributes and duck-typed fake APIs keep working.
  Guards are flexible staticmethods (accept an optional leading `self`) because
  tests borrow `CloudAPI._compare_guard` onto fake classes.

## Safety semantics (unchanged)
Immutable operation dirs (intent.json compared as Python objects), frozen
prepared.json / draft.xml / compare-request.gz receipts, `.mutation-claim`
written with O_EXCL before the single POST/PATCH, no automatic retry of an
ambiguous write, published.json → reconcile/readback, write_uncertain with
`ambiguous=True` on any doubt. CloudAPI.reconcile routes cloud_page_ops.KINDS
to cloud_page_ops.reconcile and erase/move kinds to cloud_ink_erase.verify_readback.
CloudAPI.reconcile also routes cloud_shapes.KINDS (`add_shape_v1`) to
cloud_shapes.reconcile.

## cloud_shapes (2026-10-27)
`cloud_shapes.rs`: `shape_intent`, `add_shape`, `reconcile`. Reuses the
cloud_page_ops transaction helpers (`begin`, `plan`, `expected`, `page_order`,
`compare`, now `pub(crate)`) and `reconcile_kinds` (cloud_page_ops readback
parameterized by KINDS). The offline builder (`normalize_shape`,
`prepare_add_shape`, `ShapeError`, `SHAPE_RECIPE`) is called through the live
`samsung_notes.page_shapes` module (Python today, rs-ops once ported), so this
crate has no rs-ops page_shapes dependency. Guard: `api._compare_ink_guard`
(page, layer, new object, released cache files); upload = noteResource only.
Oracle: tests/test_cloud_shapes.py (CloudShapeTests pass under rust and
rust-strict).
Byte-compat probes (`_probe_*` in the cloud_api submodule, not exported) were
compared with CPython: 3000 random JSON values ×3 styles + loads round trip,
gzip at 5 sizes, ET.tostring on namespaced/CDATA/PI/escaping inputs, uuid5;
folder/note-list parsing incl. base64/int edge cases: 0 mismatches.

## Remaining PYFALLBACKs (2 textual sites, both auth-owned)
- Login/renewal (ported last, by the auth agent): `_credential_lock`,
  `_replace_credential`, `_uat_context`, `_auth_request`, `renew_notes_token`,
  `_renew_notes_token_locked`, `renew_cloud_token` call the Python methods from
  a private copy of cloud_api.py (`samsung_notes._cloud_api_python_auth`).
- Real network: `notes_cloud.send_once` when no transport is injected.

## Cross-area calls (`src/areas.rs`)
- rlib, pure: rs-doc `text_value`, `new_document`, `parse_document` (stored
  drafts), `prepare_edit`, `multipart`, `page_inventory`, limits
  (`pyapi::doc_limits/payload_limits`), `mime::parts` (opaque fingerprints),
  `payload::parse_payload` (publish resume compare); rs-editor
  `edit::editor_content_fingerprint` / `title_content_fingerprint`; rs-ops cores
  `page_style::prepare_page_background`, `page_template::prepare_pdf_pages`,
  `page_ops::prepare_{add,delete,reorder}_page(s)`, `ink_erase::prepare_ink_{delete,move}`,
  `expected_stroke_parts`, `check_compare_response`,
  `ink_first_stroke::{first_stroke_style, prepare_first_stroke, FIRST_STROKE_RECIPE}`
  (driven through `rs_ops::bridge::PyDeps`). Core results become
  `types.SimpleNamespace` views with the same attributes the Python edits had.
- Python-facing objects go through the area's live module (the Rust
  implementation under NOTES_IMPL=rust), never this cdylib's own pyclass or
  static exception copies: `document.DocumentSnapshot`/`parse_document`
  (snapshots handed to callers), `editor_document` model/projection/edit
  functions, ink `StrokeSamples`, `normalize_capture`, `prepare_ink_append*`,
  `prepare_ink_clone`, `InkError`, pen-size limits. Errors are raised as the
  live classes (`rs_doc::pyapi::document_error/payload_error`, `rs_ops::bridge::to_py`).
- Strict-import note: with `tools.notes_cloud` now mapped to the auth area,
  the global strict import of cloud_api fails until auth exports notes_cloud;
  cloud-scoped strict (other areas `rust`) passes 159/159.

## Known deviations (fail-closed, never seen in Samsung data)
- `int()` with non-ASCII Unicode digits is rejected (CPython accepts).
- JSON lone-surrogate escapes are rejected on load (Python keeps them).
- XML input must be UTF-8 (roxmltree); other declared encodings fail to parse.

## Perf
Guards/parsing/serialization run in Rust; orchestration cost is dominated by
the doc/editor fallbacks and network. No measurable regression in the suites
(157 cloud tests ~9 s, same as Python).
