# rs-store — Rust port of the SQLite library/outbox

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Python modules replaced (all public names exported, strict-import OK):
`storage` (now_ms, private_directory, normalize_commit, StoreError, Store),
`organization_store` (favorite_from_document, migrate, OrganizationStore),
`editor_store` (EditorStore), `ink_store` (samples_from_points, validate_batch,
InkStore), `preferences` (3 functions), `shortcuts` (3 functions).

## Layout
- `val.rs` — JSON value model + CPython-compatible `json.dumps`/`json.loads`
  (separators, `ensure_ascii`, float `repr`, NaN/Infinity, duplicate keys) and
  `str.casefold` (table generated from CPython 3.14, `casefold_table.rs`).
- `db.rs` — `Db` over rusqlite with CPython `sqlite3` legacy transaction
  semantics: DML implicitly opens a deferred transaction, explicit
  `BEGIN IMMEDIATE`, commit/rollback at `with` exit, `executescript` commits
  first; same PRAGMAs/timeout as `Store.connect()`. Errors map to the
  `sqlite3.*Error` classes. Blocking SQLite calls release the GIL (CPython does
  the same) so busy waits never stall the thread holding the competing lock.
- `storage.rs`, `organization_store.rs`, `editor_store.rs`, `ink_store.rs` —
  pure-Rust core (`StoreCore` + free functions), no PyO3 types. Schema, SQL text,
  transaction boundaries, StoreError codes/messages/status, file modes (dir 0700,
  db 0600) and JSON bytes are identical to Python.
- `host.rs` — `Host` trait: overridable store methods + other-area collaborators.
- `pyconv.rs` — StoreError (ValueError subclass; `args == (message,)`), the
  sqlite3-like `Connection`/`Cursor`/`Row` returned by `Store.connect()`
  (context manager, `execute/executemany/executescript/commit/rollback`,
  `fetchone/fetchall/iter/rowcount`, Row by index or case-insensitive name,
  `dict(row)`, `tuple(row)`), duck-typed foreign connections for `migrate(db)`.
- `pystore.rs` — `#[pyclass(subclass, dict)] Store` with every method of
  Store + OrganizationStore + EditorStore + InkStore, and `PyHost`.

## Behaviour notes
- One `Store` class replaces the mixin chain; `OrganizationStore`,
  `EditorStore`, `InkStore` are exported as empty marker classes so
  `class Store(InkStore, EditorStore, OrganizationStore)` in storage.py still
  executes before the override.
- Monkeypatching: internal calls to `_apply_document_tx`, `_apply_favorite_tx`,
  `guard_ink_job`, `ensure_no_active_ink`, `organize_cloud` (from organize) and
  `apply_document` (from resolve) dispatch through Python when the instance
  `__dict__` or a subclass overrides them (the hook gets a `Connection` sharing
  the same transaction); otherwise native. Other internal self-calls are native.
- Opens databases created by the Python version and vice versa (verified in all
  four create/open combinations; sqlite_master, metadata JSON and row contents
  byte-identical after a scripted scenario).
- Linked against the system libsqlite3 (the one CPython's `_sqlite3` uses) so
  only one SQLite copy is in-process; feature `bundled` for standalone builds.
- Known divergences (edge only): lone surrogates in input strings become U+FFFF
  (still rejected by text validation; folder names with lone surrogates are not
  rejected); `favorite_from_document` parses only UTF-8 XML (roxmltree);
  `Store.list(filter=None)`/`create(None, …)` treat explicit None as default;
  Connection does not support named (`:name`) parameters.
- MIN/MAX_PEN_SIZE (0.5/128.0) are constants in host.rs mirroring ink_coordinates.

## PYFALLBACK: 0

- ink append dry runs (recipes 2 and 3) call `rs_ink::core::prepare::append_v2`/`append_v3`.
  The `AppendSnapshot` is built from the Python DocumentSnapshot's resource_xml, raw_body,
  content_type and commit_id, `RawSamples::from_parts` builds the samples, and
  `rs_doc::pyapi::payload_limits(py)` supplies the limits. Errors go through
  `rs_ink::pyapi::ink_error`.
- With non-iterable `pageIds`, delete/reorder calls `_ops::page_ops::load` first and then
  raises the TypeError, keeping Python's order.

The remaining collaborators also call STABLE rlibs directly:
- rs-ops core with `_ops::bridge::PyDeps`: page_background_state,
  prepare_page_background, prepare_pdf_pages, page_ops_state, prepare_add/delete/reorder,
  prepare_ink_delete, prepare_first_stroke. The state dicts are built exactly like the
  rs-ops wrappers build them.
- rs-editor: normalize_model.

Boundary class lookups, not counted (no logic runs in Python): `samsung_notes.document.DocumentSnapshot`
(`from_dict`), `samsung_notes.ink_editor.StrokeSamples`, and the exception classes
DocumentError, InkError and PayloadError.

## Recipe 4 (first stroke)
validate_batch sends a single stroke without `sourceStrokeId` to `validate_first_stroke`,
which runs the rs-ops `prepare_first_stroke` dry run. save_ink accepts that batch even when
`appendInk` is False, and ink_job accepts recipeVersion 1–4.

## Perf
fsync-dominated (synchronous=FULL): get+list x300 0.57s vs 0.91s Python;
apply_document (200 KB body) x100 0.46s vs 0.48s.
