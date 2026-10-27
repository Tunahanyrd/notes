# rs-auth — Rust port of the `auth` area (login / session / network path)

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Extension `_auth`; submodules named after the Python modules they replace. This
is the **last** area. Every public name of the auth-area modules is now in Rust
and the whole suite passes under global `NOTES_IMPL=rust-strict`. rs-auth uses
**no** `py_fallback` (count 0): it reuses rs-cloud's STABLE pure helpers as an
rlib (`rs_cloud::util` / `pyjson` / `glue`) and calls the Python stdlib for I/O.

## Design (why the oracles stay green and nothing hits the network)

The logic of each function is in Rust, but its **I/O goes through the exact
Python stdlib seams the oracle patches**, so the suite injects fakes and makes
no external request:

- Transport: `urllib.request.build_opener(NoRedirect(), HTTPSHandler(context=
  ssl.create_default_context()))` + `opener.open(req, timeout=…)`, imported fresh
  each call, so `patch('<mod>.urllib.request.build_opener', …)` is honored.
  A single request, no redirects, TLS verified, no cookie jar, identical headers
  /body/limits/timeouts and identical receipt + `.*-claim` files (O_EXCL, 0600).
- `notes_login.ensure_state`/`dispatch` read the module-global `STATE` and call
  `capture`/`command` via the module, honoring `patch.object(notes_login, …)`.
- `notes_auth_bridge.{authenticate,authorize_notes,exchange_notes_code}` resolve
  `perform_once`/`load_bound_inputs` from the module at call time, honoring
  `patch.object(bridge, 'perform_once'/'load_bound_inputs', …)`.
- `session.SessionManager` keeps all state in the instance `__dict__` (like
  CloudAPI), so `SessionManager.__new__` + manual attribute setting, engine
  patching and subclassing all work; `RESEARCH`, `samsung_notes.cloud_api.CloudAPI`
  and `samsung_notes.storage.StoreError` are resolved at call time.
- `CallbackError` is a real `PyException` subclass (`create_exception!`);
  `NoRedirect` is a genuine `urllib.request.HTTPRedirectHandler` subclass built
  via `type()` (PyO3 cannot `#[pyclass]` a subclass of an arbitrary Python base).

## Why this is NOT a self-contained pure-Rust TLS client

An oracle (`test_notes_cloud.test_transport_accepts_urllib_implicit_get_…`)
patches `notes_cloud.urllib.request.build_opener` and asserts `opener.open` is
called exactly once. A send_once built on an independent Rust HTTPS client would
ignore that patch — breaking the oracle **and firing a real request at
api.samsungcloud.com during the suite**. So the oracle *requires* the transport
to stay on the `urllib` seam. A standalone pure-Rust blocking HTTPS client for
the Tauri app would therefore be a *separate* component (tested offline against
local fake servers); it cannot sit on the oracle-tested send_once path without
changing that oracle. That component is **not built in this change** — see the
open item below.

## Scoreboard (all Rust unless noted)

| module | names |
|---|---|
| `session` | SessionManager |
| `notes_cloud` (and `tools.notes_cloud`) | endpoint, registration_request, changes_request, send_once, register, accept_registration, changes, scan_for_note, fetch_candidate, main |
| `notes_login` (and `tools.notes_login`) | private_write, load_json, save_json, desktop_quote, ensure_state, command, install_handler, dispatch, uninstall_handler, start, status, main |
| `probe_account_entrypoint` | save_private, NoRedirect, main |
| `notes_auth_bridge` | uat_request, perform_once, authenticate, authorize_notes, exchange_notes_code, main |
| `exchange_account_code` | safe_text, prepare, load_bound_inputs, exchange, main |
| `capture_oauth_callback` | CallbackError, validate, capture, main |
| `await_notes_login` | main |
| `samsung_notes.maetel` / `_maetel_native` | research-only (needs `unicorn`); kept Python via `ALLOW_PYTHON` in `samsung_notes/_rust.py` |

## Verification

- `python3 tools/rust_conformance.py --strict-suite`: `suite_rust` and
  `suite_rust_strict` both **tests 388/388, tools 79/79**. Every auth module is
  listed STRICT. rs-auth PYFALLBACK 0.
- Auth tool oracles under `NOTES_IMPL=rust`: 27/27.
- `tests.test_cloud_api` + `tests.test_engine_review` (renewal + SessionManager
  with the Rust `notes_cloud` builders): 28/28.

## Open item: CloudAPI renewal internals (rs-cloud), deferred — needs a decision

The CloudAPI login/renewal methods still delegate to the private Python copy
`samsung_notes._cloud_api_python_auth` from inside rs-cloud
(`renew_cloud_token`, `_renew_notes_token_locked`, `renew_notes_token`,
`_credential_lock`, `_replace_credential`, `_uat_context`, `_auth_request` —
one `py_fallback` site, `crates/rs-cloud/src/cloud_api.rs:58`).

This was **deliberately not ported in this change**, for safety:

1. It is the most credential-safety-critical code in the project: atomic
   single-use claim before every POST, no automatic replay of an ambiguous
   exchange, no rollback of a newer credential, operation journaling.
2. It already passes **every** renewal oracle via the vetted Python copy (same
   source as `cloud_api.py`, exceptions rebound to the Rust classes):
   `test_cloud_api` cached-UAT renew, completed-operation replay (no re-send),
   uncertain-exchange no-replay-on-restart, account-mismatch stop-before-network,
   terms-required, invalid-response no-rollback, accepted-token recovery,
   cloud-renew invalid no-replace.
3. **Global `rust-strict` already passes without it** — strict checks
   module-level names, and `CloudAPI` is already a Rust class; the suite gate is
   met. Porting these 7 methods only reduces rs-cloud's `py_fallback` 2 → 1
   (a purity metric for a crate outside this area), with no functional or
   strict-suite benefit.
4. I cannot live-validate renewal here (no sign-in, no tokens, no Samsung
   network), and the area's hard limit is "the live session must keep working."
   Replacing proven credential code with fresh Rust verified only by mocks is
   exactly the kind of hard-to-reverse change the project rules say to avoid.

The other rs-cloud `py_fallback` (`cloud_api.rs:356`, fetching
`notes_cloud.send_once`) is architectural: rs-cloud cannot depend on rs-auth
(cycle), so it reaches the now-Rust `send_once` through the Python module
boundary. It calls the Rust implementation; only the lookup is via Python.

Recommendation: port the 7 renewal methods in a follow-up **together with a live
token-renewal validation** by the user, editing only those methods in rs-cloud.
I can do it on request.

## MUST be live-validated by the user (cannot be checked here)

1. Full browser sign-in end to end (metadata → callback → UAT → Notes
   authorization code → Notes account token → Cloud registration):
   `session.SessionManager._login` + `notes_login.start/dispatch` +
   `notes_auth_bridge.*` + `notes_cloud.register`. These are now Rust and pass
   the mock oracles, but were never runnable without a real session.
2. Token renewal against the live server (the Python renewal path, unchanged).
3. The ms-app desktop handler install/uninstall (`notes_login.install_handler`),
   tested only against temp HOME/XDG in principle; not exercised here.

The pure request builders (`prepare`, `uat_request`, `registration_request`,
`changes_request`) are byte-checked against the oracle, but only a real
sign-in/renewal proves the live server accepts the bytes they produce.
