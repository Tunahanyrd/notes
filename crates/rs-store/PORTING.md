# rs-store — Rust port of the SQLite library/outbox

## Schema 7 deployment and recovery

Before starting a schema 7 engine on an existing schema 6 library, the deploy
operator must stop the engine, hold its exclusive `engine.lock`, and confirm no
job is `running`. Do not retry ambiguous operations. Check that free disk space
is at least 1.2 times the database size. Take a SQLite **online backup**, including
the WAL state, to a private `backups/before-v7-<timestamp>.sqlite3` file; run
`PRAGMA integrity_check` on that backup and record the pre-migration draft count.
Keep the backup until the migrated library and its projections have been
verified. Copying only the main SQLite file is not a valid backup of a live WAL
database. The migration does not create this backup; deployment owns it.
After the backup is checked, engine startup retains `engine.lock` and starts
the loopback server before storage preparation. During migration, authorized
`/api/status` requests return `migration.state`, `completed` and `total`; other
API operations return 503. The desktop shows progress and starts normal work
only after the ordinary status returns. Startup does not create the backup;
deployment must complete it first.

For each note and active-job snapshot, migration checks the source digest and
that its resource XML is exactly the parser output for the original multipart,
then verifies byte-for-byte reconstruction before publication. It stores a
versioned hash of all remaining snapshot metadata and of each note's page index
in the schema transaction. After commit it reopens the containers and verifies
the reconstructed raw digest, metadata and page indexes before VACUUM. Exact
raw identity and the pinned parser recipe imply the same resource and editor/
ink projections without recomputing those expensive projections twice. Older
pending migrations retain their original projection-hash verification. A
mismatch, missing baseline, or incompatible recipe leaves the pending marker
in place and blocks completion; restore the backup or rerun with the same
verifier after correcting the cause.

Container publication precedes the SQLite commit, so interrupted work may
leave unreferenced files. Retrying from schema 6 preserves every snapshot;
collection may later remove orphans after the grace period. The migration then
checkpoints WAL, runs `VACUUM INTO` a private file, verifies the copy, and
atomically replaces the database. A durable pending marker makes a rerun
finish reclamation if interrupted after the schema commit or replacement. Keep the
verified pre-migration backup until the new library and its projections have
been validated. Roll back by restoring that backup with the engine stopped; do
not use an old engine on a schema 7 database.

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

## Standalone document store

`document_store` is a reusable component and is **not connected to the notes
database or outbox**. The engine still uses schema 6. `DocumentStore::open`
creates a private `documents` directory under the supplied private directory.
`DocumentRef` contains source and stored SHA-256 digests and byte sizes; the
source digest names the file at `documents/ab/cd/<source_sha256>`. Files are
zstd frames. `DocumentStore::read` verifies the stored and decompressed bytes
and sizes. Streaming decoding caps its window at 8 MiB; accepted decoded and
stored sizes are capped at 256 MiB and 260 MiB respectively. Reads return a
buffer, so the decoded-size limit is also an upper bound on returned bytes;
the zstd library's total allocator usage is not independently capped.

Call `with_write` and use its `DocumentWriter::put` or `put_reader` to publish
a file. The callback **must include the entire transaction that publishes or
removes its references**, and all cooperating processes must use the same
store lock for such transactions. A successful put has synced its file and
directory entries before it returns. A failed reference transaction may leave
an orphan. Never nest lock-taking methods for the same store.

`doctor` and `collect` take a callback that enumerates every live reference;
the callback runs only after they acquire the same cross-process lock. It must
query the authoritative current reference set inside that callback, not use a
snapshot taken before lock acquisition. Doctor verifies both hashes and reports
counts only. Collection aborts on an enumeration error or malformed source hash,
and removes only unreferenced canonical files and interrupted temporary files
older than the caller's grace period. It never follows symlinks or removes
unrecognized names. These guarantees require every writer and collector to
honor the lock. `read` alone does not hold the lock; use the writer guard's
`read` when a reference lookup and subsequent read must be protected together.
