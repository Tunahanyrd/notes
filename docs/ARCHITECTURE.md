# Architecture

This document describes how the client is put together. For the contribution
workflow and the rules that protect users' notes, read
[CONTRIBUTING.md](../CONTRIBUTING.md) first.

## Components

```
┌──────────────────────────── desktop process (Tauri 2) ────────────────────────────┐
│  WebView: React UI (desktop/src)                                                  │
│     │  fetch() + Authorization: Bearer <per-launch token>                         │
│     ▼                                                                              │
│  http://127.0.0.1:<random port>  ◀── engine: child process or in-process          │
└───────────────────────────────────────┬───────────────────────────────────────────┘
                                        │
              ┌─────────────────────────┴─────────────────────────┐
              │ engine (notes-engine, pure Rust)                   │
              │  - local HTTP API (server)                         │
              │  - sync / outbox / job runner (engine)             │
              │  - SQLite library + preview cache (store)          │
              │  - sign-in, token renewal (login/session)          │
              └───┬───────────────┬───────────────┬────────────────┘
                  │ HTTPS         │ subprocess    │ subprocess
                  ▼               ▼               ▼
        Samsung account /   PDF renderer     ink renderer
        Samsung Cloud       (poppler, in     (tools/ink-renderer,
                            sandbox)         GPL-3.0, in sandbox)
```

- The **UI** never holds Samsung credentials. It receives bounded DTOs from
  the local API: note lists, editor models, ink projections without raw
  binaries, preview images.
- The **local API** binds to loopback only, requires the bearer token written
  to a private runtime file, and validates Host/Origin headers, body size and
  JSON before dispatching.
- The **engine** owns all state and every network call.

## Crates

The engine is pure Rust; the application contains no Python. Python 3
(standard library only) is used solely by helper scripts: `tools/branding.py`,
`tools/third_party_licenses.py`, the public-export tool and the conformance
harness `crates/notes-engine/conformance/run.py`.

Area cores (`crates/rs-*`): plain-Rust implementations of each area — `doc`
(note payload/XML), `editor` (rich text), `ink` (stroke binary, capture,
projection), `ops` (offline document operations: pages, erase, move, shapes),
`render` (previews, PDF/ink renderer drivers, sandbox), `store` (SQLite
library and outbox), `cloud` (Cloud transactions), `engine` (HTTP dispatch
core), `auth` (sign-in pieces), with shared helpers in `rs-common`. Each crate
also has PyO3 bindings behind a default-off `python` feature; they are not
part of the product and CI checks that PyO3 never reaches `notes-engine`.

Engine crates:

| Crate | Role |
|---|---|
| `notes-protocol` | Wire-level protocol and crypto helpers shared by the CLI and engine |
| `notes-cli` | Small command-line helper (e.g. sign-in callback handling) |
| `notes-host` | Error kinds, `DocumentSnapshot` form, pure host implementations, area facades |
| `notes-cloud` | Cloud transaction layer: compare, update, readback, page/ink operations, token renewal |
| `notes-net` | Blocking HTTP/1.1 + TLS client with wire behaviour identical to the reference |
| `notes-login` | Browser sign-in, callback capture/decryption, registration, `ms-app` scheme handler |
| `notes-preview` | Page previews and thumbnails, PDF signed-link refresh |
| `notes-platform` | Operating-system seams (directories, opener, process limits, tools) for Linux and macOS |
| `notes-engine` | Engine, sessions, HTTP dispatch, launcher; binary `notes-engine` and an in-process launcher for Tauri |

Separate GPL-3.0 workspaces (own `Cargo.lock`, not workspace members):
`tools/ink-renderer` (the `ink-render` CLI used for saved-page previews) and
`crates/notes-ink-live` (the same sdocx stroke renderer compiled to
WebAssembly for the live pen preview in the UI). Both link sdocx from
`references/upstream/sdocx`, fetched at a pinned commit by
`tools/fetch_sdocx.sh`.

Behaviour is pinned, not assumed:

- `cargo test --workspace` covers the crates, including recorded protocol
  exchanges and differential fixtures;
- `crates/notes-engine/conformance/golden/` holds a synthetic input library
  and the frozen responses of the earlier reference implementation (status
  line, headers, body, plus the final SQLite tables and written files).
  `run.py --replay --candidate target/debug/notes-engine` replays the same raw
  HTTP request sequence against the engine and compares byte for byte
  (generated ids, timestamps, roots and ports masked). The required result is
  100 %. An intended behaviour change re-records the goldens with
  `run.py --record --reference target/debug/notes-engine`; the golden diff is
  reviewed like code.

## Storage

One SQLite library per data directory (XDG state directory by default).
Schema version 6 (`SCHEMA_VERSION` in `crates/rs-store/src/organization_store.rs`).
Main tables:

- `metadata`, `notes` — cached Cloud documents and per-note state;
- `jobs` and the per-kind job tables (`editor_jobs`, `ink_jobs`,
  `ink_erase_jobs`, `ink_move_jobs`, `page_op_jobs`, `page_style_jobs`,
  `pdf_page_jobs`, `organization_jobs`, `folder_jobs`) — the outbox;
- `folder_nodes`, `editor_documents`, `ink_documents`, `ink_drafts`.

Migrations run on open; before migrating a real library, take an SQLite online
backup (never copy a live database together with its WAL files).

## Sync and the outbox

```
download:  Cloud change list ──▶ fetch changed notes ──▶ parse/validate ──▶ SQLite (one transaction)

upload:    UI action ──▶ local API ──▶ outbox row (immutable snapshot + operation id)
                                         │
                             job runner claims the row (queued → running)
                                         │
                  GET current Cloud revision ── differs? ──▶ conflict, stop
                                         │ same
                              build the update offline
                                         │
                                 single POST / PUT
                                         │
                 outcome unknown? ──▶ mark uncertain, never auto-retry
                                         │ accepted
                          full readback of the note, compare
                                         │ matches
                           checkpoint in SQLite, next step
```

- Operation ids are immutable; ink strokes use deterministic UUIDv5 ids, so a
  checkpointed stroke is never sent twice.
- Multi-stroke ink batches checkpoint after each verified stroke; cancel and
  conflict are checked before every stroke, checkpoint and acknowledgement.
- While an ink job is active, title/body/organization writes to that note are
  blocked.
- "Saved locally", "uploaded" and "verified" are separate states all the way
  to the UI.

## Ink path

1. The UI captures pointer input (`InkEditor.tsx`) in page coordinates and
   keeps the draft in memory, persisting it to `ink_drafts` after each stroke.
2. `PUT /api/notes/:id/ink` validates the batch (limits on strokes and
   points) against the current revision.
3. The engine copies style from a supported stroke on the same page (or a
   built-in recipe for the first stroke on an empty page), encodes the stroke
   in the note's binary format and scales it to the page owner's coordinate
   space.
4. The outbox runs the compare / POST / readback cycle above.

Format notes: `docs/EDITOR_FORMAT.md`, `docs/INK_*.md`.

## Previews and sandboxing

Page images come from, in order of preference: images already saved in the
note, PDF backgrounds rendered with poppler, and ink reconstructed by
`tools/ink-renderer` (a separate Rust CLI that links the GPL-3.0 sdocx library
at a pinned commit; it is a separate program, communicating through pipes).

Every renderer runs as its own process with resource limits (address space,
CPU time, file size, no core dumps), bound to the parent's lifetime, and — on
Linux — inside `bwrap --unshare-all` with read-only binds of only what it
needs. Where bubblewrap cannot run (inside Flatpak, where a nested user
namespace is not available, and on macOS) the engine falls back to a reduced
sandbox: the same limits and process isolation without namespaces. Previews
are cached per note revision; a priority queue serves the open page before
thumbnails and warm-up work.

## Sign-in

1. The engine opens the Samsung account sign-in page in the user's browser.
2. The account service redirects to an `ms-app://` callback URL. The client
   registers a handler for `x-scheme-handler/ms-app` (a desktop entry; the
   Flatpak declares it in its own desktop file and receives it through the
   portal) whose command passes the URL to the engine
   (`notes-engine __notes-login-callback`).
3. The engine validates and decrypts the callback, exchanges the code for
   tokens and stores the session privately in the data directory.
4. Tokens are renewed through the account service when they expire.

## Platforms

- **Linux**: primary platform. Packages: AppImage, .deb, .rpm (Tauri bundler)
  and Flatpak (`packaging/flatpak/`).
- **macOS**: port in progress (`notes-platform` macOS branch).
- The engine also runs headless (`notes-engine --frontend-dir desktop/dist`)
  and serves the UI to a browser, which the tests use.

## Product name

The product name, application id and binary name come from `branding.json`;
packaging files are generated by `tools/branding.py`.
