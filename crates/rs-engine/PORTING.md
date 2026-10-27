# rs-engine: porting status

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Area `engine`: `samsung_notes/engine.py`, `server.py`, `ui_assets.py`, and the
`python -m samsung_notes` launcher (`__main__.py`). Oracle: the unchanged Python
suites (`tests/test_app_server.py`, `test_engine_review.py`,
`test_title_application.py`, `test_editor_application.py`,
`test_cloud_page_style.py`, `test_cloud_pdf_pages.py`, `test_ink_drafts.py`,
`test_ui_assets.py`, plus every suite that builds an `Engine`/`AppServer`).

## What is ported

| Python | Rust | Notes |
|---|---|---|
| `engine.Engine` | `engine.rs` (`#[pyclass(subclass, dict)]`) | every method, incl. `export_file`, `page_ops`, `save_page_op` (op `shape` dispatches `cloud_shapes.add_shape`), `shapes`, `save_ink_erase`, `ink_geometry`, `_push_ink_erase`, `templates`, `apply_template`, `add_pdf`, `_fetch_many` (Rust threads, input order kept, first error re-raised after all finish like `ThreadPoolExecutor.map`) |
| `server.AppServer`, `server.Handler` | `server.rs` (PyO3 adapter) + `server_core.rs` (pure) | own HTTP/1.0 server; accept loop runs without the GIL; one thread per connection; routes include `GET /shapes` and page-ops op `shape` ({op,pageId,shape,expectedRevision}) |
| `server.MAX_REQUEST`, `PREVIEW_WAIT_SECONDS` | `server_core.rs` constants | also exported |
| `ui_assets.local_font` | `ui_assets.rs` (pure + thin `#[pyfunction]`) | O_NOFOLLOW private read, manifest sha256/bytes check |
| `__main__.main` | `launcher.rs` (exported as submodule `__main__`) | argparse-like CLI, umask, private data dir, `engine.lock` flock, 0600 + fsync runtime file, launch HTML, SIGTERM/SIGINT -> shutdown |

Pure-Rust layers (no PyO3 types; linkable from Tauri):

- `pyjson.rs` — CPython `json` compatible reader/writer: duplicate keys and
  NaN/Infinity rejected in API mode, lone `\udXXX` escapes kept (as UTF-16, so
  the store raises its own `invalid_text` 400 exactly like Python), 4300-digit
  int limit, UTF-16/32/BOM detection, `", "`/`": "` separators, Python float repr.
- `pyhttp.rs` — `BaseHTTPRequestHandler` wire behaviour: request line rules
  (414/400/505/501, HTTP/0.9 body-only replies), `http.client.parse_headers`
  limits (65536-byte lines, 100 headers -> 431), `email` compat32 header
  parsing (folding, `get`/`get_all`, `get_content_type`), `Server: LinuxNotes `,
  `Date`, Python's HTML error page and reason phrases.
- `urlparse.rs` — `urlsplit`, `unquote` (replace/strict), `parse_qs` with
  `max_num_fields` (ValueError -> 500 like Python), `quote`.
- `mime.rs` — `mimetypes.guess_type` (CPython 3.14 defaults overridden by
  `/etc/mime.types`, suffix/encoding maps). Tables generated into `pytables.rs`.
- `server_core.rs` — `Backend` trait, `Config`, validation (Host exactly once,
  Origin, `Sec-Fetch-Site`, constant-time bearer compare), body limits,
  routing of every `/api/*` route, static files (`os.path.realpath` semantics,
  traversal check, SPA fallback, charset rules), local fonts, `dispatch`
  exception mapping.
- `engine_core.rs` — UUIDv5 operation IDs (own SHA-1), template discovery from
  note XML (roxmltree; binascii-lenient base64), PDF staging (0600 atomic),
  export naming/target/`magick` pipeline, `normalize_commit`, `now_ms`,
  `private_directory`.

### Verification done

- `NOTES_IMPL=rust`: the oracle suites above pass (36 + 123 targeted tests);
  full suites 2026-10-26: `tests` 386/386 OK, `tools` 79/79 OK.
- `cargo test -p rs-engine --lib`: 6 pure-Rust unit tests (JSON, HTTP parsing,
  urllib, UUIDv5 vs CPython, argparse/runtime file).
- Latest (templates cache + ink-move): `tests` 396/396 and `tools` OK under both
  NOTES_IMPL=rust and NOTES_IMPL=rust-strict.
- Differential raw-socket check (Python `Handler` vs Rust server on the same
  synthetic offline engine): 95 requests incl. malformed request lines, header
  limits, HTTP/0.9, auth/host/origin variants, bad bodies, traversal, symlink
  escape, MIME types, HEAD — byte-identical except for freshly generated note IDs.
- `export_file` (txt) identical names/content/collision suffixes vs Python.
- Launcher smoke test on a synthetic data dir: runtime file 0600 with
  port/url/token/pid, second instance refused (exit 1), SIGTERM -> clean exit 0
  and runtime file removed, `--port 99999` -> argparse-style error.

## Remaining PYFALLBACKs (1 call site)

- auth: `session.SessionManager` (until the auth area is ported).

STABLE areas (store, doc, editor, ink, ops, cloud, render) are reached through their
`samsung_notes.*` modules (`engine.rs::area`, `server.rs` preferences/shortcuts):
under NOTES_IMPL=rust that resolves to their Rust implementations, and the
cross-extension rule (never use another cdylib's pyclasses directly) holds.
Recipe 4 (`cloud_page_ops.append_first_stroke`) is dispatched in `_push_ink_batch`;
`move_ink` jobs go through `_push_ink_move` (`cloud_ink_erase.move_ink_strokes`, op id
`uuid5(job, 'move-<i>')`), `pendingMove` appears in `ink()` only while a move is queued.
`templates(note_id=None)` scans `json_extract(base_document,'$.resource_xml')` through the
static `Engine._scan_templates` (looked up on the class, patchable), caches
`(revision, entries, file_ids)` per note in `_template_cache`, and adds `inNote` with
`note_id`; `GET /api/templates?note=<uuid>` validates like server.py (bad -> 400,
more than one field -> 500 like `parse_qs(max_num_fields=1)`).

Exception classes (`StoreError`, `APIError`, `ConflictError`, `DocumentError`,
`PdfBackgroundError`) are imported from their Python modules at call time (as
the brief allows), so Rust replacements in other areas stay `isinstance`-compatible.
`storage.normalize_commit`, `now_ms`, `private_directory` are re-implemented
in `engine_core.rs` (tiny, identical behaviour) instead of falling back.

## Behaviour notes

- Engine state is kept in the instance `__dict__` and every internal
  `self.x(...)` goes through Python attribute lookup, so tests that patch
  `engine._api`, replace `engine.previews`, set `api_session`, or patch store
  methods behave as with the Python class. Threads (`worker`, `warmup`) and
  locks/events are still `threading` objects for the same reason.
- `__init__` takes `metadata_factory` through `**kwargs` so an explicit `None`
  (no metadata sync) is distinguished from "not given" (CloudMetadata).
- `_push_ink_batch` evaluates `append_ink_stroke_v2/_v3/append_ink_stroke`
  eagerly like the Python dict literal.
- Known, documented divergences (edge cases only): `urlsplit` skips CPython's
  NFKC netloc check for non-ASCII absolute-form targets; `str.isdigit` for
  non-ASCII digits is approximated; the JSON nesting limit is fixed at 60 000
  (CPython's is stack dependent, 50k–100k); symlink-loop handling in
  `realpath` is approximate; `mimetypes` ignores the Windows registry (not used on Linux).
- `samsung_notes/__main__.py` has no `_rust_override(...)` call, so
  `python -m samsung_notes` still runs the Python `main()` even under
  `NOTES_IMPL=rust` (the Python `main` then builds the Rust `Engine`/`AppServer`).
  The Rust `main` is exported as `_engine.__main__.main`; adding the hook line to
  `__main__.py` (lead's call) switches it.
- Global `NOTES_IMPL=rust-strict` imports currently fail before reaching my
  modules (`tools.notes_payload`, doc area, is unported), so strict suite runs
  are blocked by other areas; the scoreboard shows 1/1, 2/2, 1/1 names ported.

## Performance notes

- The accept loop and the wait in `shutdown()` run with the GIL released; each
  connection thread only takes the GIL for the engine call itself. Request
  parsing, JSON body decoding, validation, static/font file serving and response
  writing are GIL-free. Static files and fonts never touch Python.
- Response JSON is produced by `pyconv::from_py` + `pyjson::dumps` (no Python
  `json` module).
- `_fetch_many` uses up to `DOWNLOAD_WORKERS` Rust threads; `get_note` releases
  the GIL during network I/O, so downloads overlap like the ThreadPoolExecutor.

## Plan: Tauri hosts the engine in-process (do not edit desktop/src-tauri yet)

Goal: `linux-notes-desktop` stops spawning `python3 -m samsung_notes`; the React
UI keeps talking to the same loopback HTTP API (same routes, token, Host/Origin
checks, CSP), so no frontend change is needed in the first step.

1. **Phase 0 (now possible): embedded server, Python engine.** Add `rs-engine`
   as a path dependency of `desktop/src-tauri` with PyO3 `auto-initialize`
   (separate feature from `extension-module`; add a `embed` cargo feature to
   rs-engine that switches pyo3 features). In `setup()`: initialize Python, put
   the project root on `sys.path`, build the engine via
   `samsung_notes.engine.Engine(data_dir, offline=..., auto_sync=...)`, then
   start `server_core` directly from Rust: bind `127.0.0.1:0` (same
   `bind_ipv4` without SO_REUSEADDR), `Config { token: token_urlsafe(32), port,
   static_dir: realpath(desktop/dist), font_dir: research_dir()/ui-reference/fonts }`,
   `PyBackend { engine }`, and an accept thread calling `serve_connection`.
   Replace `launch()`'s runtime-file polling with the in-memory `(port, token)`
   — the URL `http://127.0.0.1:<port>/#token=<token>` and its validation stay
   identical. Keep the `engine.lock` flock and `private_directory` logic from
   `launcher.rs` (factor `fn prepare_data_dir()` + `fn start_server()` out of
   `main()` into pure functions first). On `RunEvent::Exit`: `Shared::shutdown`,
   drop the listener, call `engine.close()`.
   Wins: no child process, no runtime file in /tmp, no PDEATHSIG dance; the
   Python interpreter still runs the lower areas.
2. **Phase 1: replace `PyBackend` piecemeal.** `Backend` is the seam: a
   `RustBackend` implements `json(Call)` by matching `(Target, method)` to Rust
   engine methods as lower areas become STABLE (store/doc first: `GET /api/notes`,
   `/api/folders`, `/api/preferences`, `/api/shortcuts`, `GET note/editor/ink`).
   Unported calls delegate to the inner `PyBackend`, so the app works at every
   step. `Call`/`Arg` already carry JSON (`pyjson::JValue`) without PyO3 types.
3. **Phase 2: pure-Rust `Engine`.** Turn `engine.rs` logic into an
   `EngineCore<S: StoreApi, C: CloudApi, M: MetadataApi, P: PreviewApi>` with
   the same method set (the PyO3 class becomes a wrapper used only by the Python
   suites), std threads + `Condvar` for `wake/stop`, `Mutex` for state. Keep
   the outbox contracts verbatim: UUIDv5 operation IDs (`engine_core::uuid5`),
   single POST, full readback, never auto-retry ambiguous writes, cancel/claim
   race handling in `_push`.
4. **Phase 3: drop Python.** When `session.py` (auth area) is ported, remove the
   embedded interpreter; Tauri links only Rust crates. Optionally replace the
   loopback HTTP with Tauri IPC later, behind the same `Backend` trait, once the
   React client has an IPC transport (out of scope for the first migration).

Risks to handle in the Tauri step: Python finalization while connection threads
are attached (shut the server down and join before finalizing), packaged builds
must ship `samsung_notes/` + numpy until Phase 3, and the GPL-3.0 sdocx renderer
obligations are unchanged.
