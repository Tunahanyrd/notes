# Contributing

Thanks for helping. This project writes to people's real notes in a cloud
service it does not control, so correctness and privacy rules come first.
Please read this file before opening a pull request. Deeper design notes are
in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Architecture in one minute

```
React UI (desktop/src)  ──HTTP + bearer token──▶  engine on 127.0.0.1  ──HTTPS──▶  Samsung account / Cloud
      ▲   Tauri window (desktop/src-tauri)              │
      └──────────────── starts / embeds ────────────────┘
                                                        └─ SQLite library, outbox, preview cache
```

- **UI**: React + TypeScript in a Tauri 2 window. It only talks to the local
  engine API; it never sees Samsung tokens, signed URLs or raw note binaries.
- **Local API**: loopback HTTP with a per-launch bearer token, Host/Origin
  checks, body-size limits and JSON validation.
- **Engine** — pure Rust (since 2026-10-27 the product contains no Python
  logic): `crates/notes-*` (the `notes-engine` binary and its crates) on top
  of the plain-Rust cores in `crates/rs-*`. The `rs-*` PyO3 bindings compile
  only with their default-off `python` feature (not product). The earlier
  reference implementation is not part of this repository; its behaviour is
  preserved as golden fixtures (`crates/notes-engine/conformance/golden/`)
  that the engine must keep matching byte for byte.
- **Renderers**: PDF pages (poppler) and ink (`tools/ink-renderer`, GPL-3.0,
  links sdocx) run as separate, resource-limited, sandboxed processes.

## Getting set up

```sh
tools/fetch_sdocx.sh                       # GPL-3.0 renderer library at the pinned commit
cargo build -p notes-engine -p notes-cli
cargo build --release --manifest-path tools/ink-renderer/Cargo.toml
(cd desktop && npm ci)
```

Or use the container in [docs/DOCKER.md](docs/DOCKER.md), which has every
system dependency (poppler, librsvg, ImageMagick, bubblewrap, Tauri libraries,
Playwright Chromium).

## Tests gate every change

All of these run in CI and must pass:

| Suite | Command |
|---|---|
| Rust crates | `cargo test --workspace` (default features: no PyO3) |
| HTTP conformance | `python3 crates/notes-engine/conformance/run.py --replay --candidate target/debug/notes-engine` (golden fixtures, Python 3 stdlib only) — must report **100 %** (every request byte-equal or masked-equal, no table/file mismatch) |
| Desktop | `cd desktop && npx tsc --noEmit && npm run build && npx playwright test` |
| Live pen preview | `cargo test --manifest-path crates/notes-ink-live/Cargo.toml` and a `wasm32-unknown-unknown` release build (separate GPL-3.0 workspace) |
| Branding / licenses | `python3 tools/branding.py sync --check`, `python3 tools/third_party_licenses.py --check` |

Workflow for a behaviour change:

1. Write or change a Rust test (`crates/notes-*` / `crates/rs-*`) and
   implement the change in Rust.
2. If an HTTP response or stored state changes on purpose, extend the
   conformance steps if needed, re-bless the goldens with
   `run.py --library crates/notes-engine/conformance/golden/library --record --reference target/debug/notes-engine`
   (copy the library out first; also re-record the `no-magick` variant) and
   review the golden diff in the pull request. Re-blessing is never a way to
   make a failing replay pass: the PR must explain each changed response, and
   golden changes need a maintainer's review.
3. UI changes: keep `tsc` clean and add a Playwright spec with a mocked API.

Never make a suite pass by loosening an assertion, hiding an error or adding
retries. A regression test should fail before the fix and pass after it.

## Write-safety rules (Cloud writes)

These rules protect users' notes. A pull request that weakens them will not
be merged.

- **Snapshot first.** Every pending change is stored in the local outbox as an
  immutable snapshot together with its operation id before anything is sent.
- **Immutable operation ids.** An operation id never changes meaning. Ink
  strokes use deterministic UUIDv5 ids per stroke; a new behaviour gets a new
  recipe/version instead of reusing an old id.
- **Compare guard.** Before uploading, the current Cloud revision is fetched
  and compared with the revision the change was based on; any unexpected
  difference stops the write (conflict), it is not merged silently.
- **One POST, then full readback.** Each write is sent once. Success means the
  whole note was read back and matched the expected result — an HTTP 2xx alone
  is not success.
- **Never auto-retry an unknown outcome.** If a write may or may not have been
  applied (timeout, connection reset, unclear response), it is marked
  uncertain and left for the user; it is never repeated automatically.
- **Cancel and conflict are honoured between steps.** Multi-step jobs check
  for cancellation/conflict before each step and keep verified checkpoints.
- **Do not flatten unsupported content.** Content the client cannot represent
  is rejected or kept read-only, never converted to plain text and written back.
- **Report precisely.** "Saved locally", "uploaded", and "verified by
  readback" are different states in code, UI and pull request descriptions.

## Privacy rules

- Never commit or paste tokens, cookies, authorization codes, signed download
  URLs, account e-mails or ids, server note ids, SQLite libraries, outbox or
  session files, real note payloads or titles, or screenshots of personal
  notes — not in code, tests, fixtures, logs, issues or PR descriptions.
- Tests use synthetic data only. Never point a test at a real library or a
  developer's state directory; CI runs with empty XDG directories.
- Live probes (`tools/live_*.py`) are run by hand, only against a dedicated
  test note, and their outputs stay outside the repository. Publish only
  sanitized summaries (counts, pass/fail), never raw payloads.
- No absolute home paths in committed files.

## Samsung material

Do not add Samsung binaries, APKs, app packages, icons, fonts, images,
decompiled or disassembled code, or text copied from Samsung resources.
Describe file formats and behaviour in your own words and keep only the facts
needed for interoperability (field names, numeric values, wire shapes). Short
references to a class or method name are fine; code bodies are not. UI icons
and fonts must be free (MIT/Apache/ISC/OFL or similar) — see
`desktop/assets-free/` and the third-party license list. See
[docs/LEGAL.md](docs/LEGAL.md).

## Reporting results

When describing a change, keep these evidence levels separate — one does not
imply another:

1. implemented in source;
2. synthetic / offline tests pass;
3. Cloud readback verified (controlled write to a test note);
4. seen on a device in the official app.

## Branding

The product is called "Notes" (application id `io.github.tunahanyrd.Notes`).
Never hardcode the name, application id or binary name; read them from `branding.json` (packaging files are generated by
`python3 tools/branding.py sync`). Describe the project as "compatible with
Samsung Notes"; never use Samsung trademarks as part of a name.

## Code style

- Follow the style of the file you edit; keep diffs focused.
- Python: helper scripts only (`tools/`, the conformance harness), standard
  library only; never product logic.
- Rust: no `unsafe` without a comment explaining the invariant; pin
  dependency versions as the workspace does.
- TypeScript: `strict` mode must stay clean.
- User-facing text must not show internal field names, raw ids or debug output.

## Sign-off and licensing

By contributing you agree that your contribution is licensed under
GPL-3.0-or-later. A `Signed-off-by` line (DCO) is welcome but optional.
Please follow the [Code of Conduct](CODE_OF_CONDUCT.md).
