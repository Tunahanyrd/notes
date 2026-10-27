# notes-cloud — pure-Rust Cloud transaction layer

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Port of `samsung_notes/cloud_api.py`, `cloud_metadata.py`, `cloud_ink_erase.py`
and `cloud_page_ops.py` without PyO3/Python (verified: test binaries link no
libpython, `nm -u` shows no `Py*` symbol). Oracle: the Python implementation,
compared byte for byte by `tests/differential.rs`.

**Credential renewal is ported but NEEDS LIVE RENEWAL VALIDATION by the user**
(only the Python oracle and fake transports have exercised it).

## Layout
- `api.rs` — `CloudApi` (constructor, credentials, `auth_status`, `headers`,
  `call` receipts, `changes`, `get_note`, `upload_content_file`, operation
  directory, `publish`, `reconcile`, opaque fingerprint, multipart).
- `writes.rs` — `create`, `update`, `update_title`, `create_editor`,
  `update_editor`, `update_page_background`, `add_pdf_pages`,
  `append_ink_stroke{,_v2,_v3}`, `update_ink_clone`, reconcile kind checks,
  `samples_from_points`.
- `ops.rs` — `erase_ink_strokes`, `move_ink_strokes`, `reconcile_erase`,
  `verify_erase_readback`; `add_page`, `delete_pages`, `reorder_pages`,
  `append_first_stroke`, `reconcile_page`, `compare_page_ops_guard`, `PAGE_KINDS`.
- `metadata.rs` — `CloudMetadata` (folder graph, note inventory, folder and
  organization writes), `parse_folder_changes`, `parse_note_list`,
  `document_organization`.
- `auth.rs` — `expiry`, `load_bound_inputs`, `uat_context`, the `Renewal`
  seam, `PortedRenewal` (default; `renew_cloud_token`,
  `renew_notes_token_locked`, `credential_lock` flock, `replace_credential`,
  `auth_request`) and `StubRenewal` (offline, never sends).
- `transport.rs` — `Request`/`Response`/`Transport`, `NoNetwork` default.
- `json.rs`, `util.rs`, `etree.rs`, `guards.rs` — byte-exact json.dumps
  (indent=2/compact/default), private files, gzip(mtime=0), uuid5, urllib
  quoting, endpoint allowlist, ElementTree model/serialisation, compare guards
  (copied from rs-cloud's pure modules).
- `editor.rs` — editor_document calls over the rs-editor core.

Cores used as rlibs (pure APIs only): rs-doc (parse/new/prepare_edit/mime/
payload/page_inventory), rs-editor (edit/rich), rs-ink (`core::capture`,
`core::prepare::prepare_append`/`prepare_ink_document`), rs-ops (page_style,
page_template, page_ops, ink_erase, ink_first_stroke) through
`notes_host::deps::PureDeps`, notes-protocol (`parse_note_changes`, the
in-process `notes-cli export-changes`).

## Seams
`Transport::send(directory, name, &Request, limit) -> Result<Response, Fail>`;
`writes_receipts() == false` = injected-transport semantics (`call` writes the
receipt/body/meta files), `true` = `send_once` semantics (the transport writes
them; `call` reads `<name>-response-meta.json`). Renewal requests go through
the same transport. `Random` (uuid4/token_hex/choice/token_urlsafe) and the
clock are injectable so receipts can be reproduced exactly.

## Tests
- `tests/native.rs` — native ports of key test_cloud_api cases with a Rust
  synthetic server.
- `tests/differential.rs` — replays recordings of the Python oracle: per
  top-level call the whole temp tree before/after, arguments, transport
  exchanges, clock and random draws, result/exception. Committed synthetic
  recordings: `tests/fixtures/oracle/*.json.gz` (31 tests, 191 steps; shapes: tests.test_cloud_shapes.CloudShapeTests, 21 steps). Full run:
  `python3 reference/python/generators/notes-cloud/record.py OUT tests.test_cloud_api ...
  cloud_scenarios` (run from reference/python; record.py puts its own directory,
  which holds `cloud_scenarios.py`, on sys.path), then `NOTES_CLOUD_DIFF_DIR=OUT cargo test -p notes-cloud
  --test differential`. Last full run: 626/626 replayable steps identical
  (results, exception class/code/message/ambiguous, every request line,
  header list and body, every file byte and mode); 4 steps skipped because
  the Python test passes argument types the Rust signature cannot express.

## Divergences
- Argument types are enforced by Rust signatures (`color_argb: i64`,
  `now_ms: Option<i64>`, `dx/dy: f64` for clone, ...) instead of the oracle's
  runtime `type(...)` checks; Python values that Rust cannot express are
  rejected at compile time rather than with the oracle's error.
- `clock_val`: a float clock that is exactly integral is written as an int in
  renewal receipts (`receivedUnixSeconds`); Python writes `x.0`.
- `update()` with a non-editable snapshot whose `read_only_reason` is None
  uses the default UnsupportedDocumentError message.
- rs-cloud's known deviations apply (non-ASCII digits in `int()`, lone
  surrogate JSON escapes, non-UTF-8 XML declarations).
- Real HTTPS (`send_once`) is not implemented here (crates/notes-net).
