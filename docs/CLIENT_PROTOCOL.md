# Independent client: initial protocol core

> Note: The evidence records referenced here (sanitized measurements, raw analysis output) are kept private and are not part of the public repository.

The target is a Rust core and a Tauri/React desktop application. The Rust library
handles XML and sign-in cryptography; Python research tools run a limited HTTP flow.
On Linux, user sign-in, the encrypted callback, the UAT and acquiring a Notes-specific
access token were **verified against the real server with HTTP 200**. The refresh field is a `-1` sentinel;
persistent renewal was not verified in the first stage. Later UAT renewal evidence is below.
A separate Cloud token was obtained, and the first 51 changes and
one note's metadata/compare XML were downloaded. An XML/multipart parser and a
limited plain-text writer were added: a controlled test note created from Linux was
seen on device; a text change made on the device came back to Linux under the same note ID.
The last Linux update was also seen on device after the fix that advances the server
time; the user explicitly confirmed it.
A Tauri/React text application and a persistent Python sync service now exist.
General note/pen editing and full Samsung Notes feature parity do not exist yet.
(Evidence for live sign-in and plain-text transfer with a real device is kept private.)

## Application layer — 25 October 2026

The `samsung_notes/` application service uses a SQLite library, a persistent cursor and
an outbox with immutable operation IDs. The UI reads from the real API; the Samsung
token is not handed to the UI. Launching and limits are in the [README](../README.md); the HTTP contract is in
[APP_CONTRACT](APP_CONTRACT.md).

In the first application stage, 78 application/acquisition/preview tests and 79 research tool tests passed. The frontend's synthetic
browser, real offline service and native process tests are recorded separately.
A controlled note creation and edit through the application's HTTP API were sent to the real
Cloud; in both cases the content matched on readback. After native
application launch and restart, preservation of 33/33 notes, the account and the last
controlled edit was verified. The application ran as a Wayland window.
In this application-layer experiment, no Android screen confirmation was additionally requested.

After a confirmed successful send, a failed read is reconciled read-only from the same
operation record instead of writing again. A send with an unknown outcome is not automatically
retried. Local edit version checking and copy-preservation on conflict are tested;
no atomic compare-and-swap on the server is claimed. The path that obtains a
new Notes authorization code and Cloud session from the current account UAT was verified live with HTTP 200/200/200.
The server still returned the same valid Notes token/expiry;
renewal of an expired token is not separately verified.
(Session renewal evidence is kept private.)

Favorites, note trash/restore, folders and note moves are now Cloud
operations. On a separate test note/folder created from the application's HTTP API,
11 operations passed with mandatory server readback. The user also confirmed that a real UI
trash operation reached the cloud. Permanent deletion, deleting a populated
folder and recreating a vanished folder path are not supported.
(Protocol evidence and application validation are kept private.)

Independent title writing and supported rich-text notes were also verified on the real
Cloud. The bold/italic/red text and right
alignment of the controlled rich note were confirmed by the user on the Android Samsung Notes screen;
they were also compared against the provided screenshot. Verification of this single note
does not mean all styles or the pen writer were verified.
(Editor evidence is kept private.)

Paired measurement on one large fixed preview input: 31.367 → 1.302 seconds wall
clock, 29.045 → 1.296 seconds CPU. The ID/hash/sizes of the output candidates stayed
the same; this is a single parsing measurement, not the open time for every note.

Bounded XML/multipart reading of large drawn notes opens up to 16,384 parts;
documents with more than 1,024 parts are not edited. In addition to title/text previews,
saved canvas cache files are downloaded with hash verification and decoded to PNG with a limited local
Samsung codec. This is not a document engine that redraws every drawing/PDF
object. Account headers are not carried across file redirects;
if the account or source changes, the next download/publish step stops. Details:
[APP_CONTRACT](APP_CONTRACT.md).

The controlled protocol experiments below are evidence predating the application layer.

`tools/notes_payload.py` parses bounded XML/multipart and produces a private local cache;
it is not an SDOCX or drawing renderer. `tools/notes_writer.py` only creates
new, single-page plain-text notes. `tools/notes_sync_probe.py` restricts remote
writes to the controlled note created by this client. Downloaded personal
notes were not modified. A controlled update to the same test note is verified below.

The first write-back preparation caught a second empty page coming from the device and stopped
offline; no update was sent to the server. Therefore rebuilding the original single-page
draft is not enough. The device's latest XML structure must be preserved; `noteTree` is sent as the full
inventory while data parts are selected according to the compareForUpsync result.
Android `k1.a.m/n` applies this distinction; using the same protocol prevents page loss.

## Updating the same note — verified live

`tools/notes_snapshot_update.py` is based on the latest device
snapshot of the note created by this client. The IDs and content of both empty pages, the title, metadata
and existing text markup are preserved. If the compare result requests only the body,
the body part is sent with the full noteTree; an unexpected source or a different commit
stops the update. If the request outcome is ambiguous, there is no automatic retry.

On the first attempt compare returned 200 and update returned 403/128409: the server time and the internal object
time had been taken into the same max computation. In the real Android snapshot some internal
values are 16 digits, while the root syncModifiedTime is 13 digits. A re-read after the rejection
verified that the note XML and the commit had not changed. This failed attempt
was preserved; the same operation record was not reused.

The native `k1.a.k → sync.db.t.j → entity.getServerTimestamp` and `k1.a.n` chain
shows that the root syncModifiedTime comes from the stored server time.
The first fix preserved this field as-is; for body/attributes/tree-body it incremented the maximum of the raw internal
time by one. A compare with a separate record returned 200 and update returned 201.
In the subsequent downsync 200 response:

- The text from the device was preserved exactly and the Linux line was added.
- The two page/layer sources and the full page tree were semantically unchanged.
- The local note ID was the same; commit and fileRevision matched the write result.

(The records of the rejected attempt, the fix source and the successful readback are kept private.)
At this intermediate stage, the user had not confirmed that the Linux line was visible on the device
screen. The user reported re-editing the text during this check;
the next device version was also downloaded and preserved. In a separate attempt made while the user was on the
note list, the content was again verified with 201/200, but the
GetNoteInfoView.syncModifiedTime fetched immediately afterwards had not advanced. Android `c4.f`, on an equal server
time, only updates the commit; it does not download the content. Therefore a successful content
readback alone does not prove that the device will receive the update.

Correction of the earlier inference: although the serializer preserves the stored time, before a normal local
save `WDocSaveOperation.setDocumentTimeEntity`, when keepServerTimestamp is
false, advances ServerTimestamp with `TimeManager.getCurrentTime()`.
`TimeManager` uses epoch-ms corrected by the server offset; the code that converts SDK time to
application time divides the 16-digit SDK time by 1000. Product-level
clock skew handling does not exist yet. In this session, 514 ms was observed between the server timestamp and the
local request time rounded to the second; the measurement includes network latency
and rounding and is not an exact clock skew.

The schema 5 fix uses a new epoch-ms for the root syncModifiedTime and displayModifiedTime,
and a separate raw counter for the internal body/attributes/tree-body. If the previous
successful controlled revision matches on commit, file hash, title/text and the entire ID
inventory, republishing without touching the text is supported. This way the
Linux line is not added twice. This repair was applied with compare 200 → update 201 → info 200 →
downsync 200. This time GetNoteInfoView.syncModifiedTime actually
advanced and matched the prepared time; the text, both pages and the new commit were
verified again. While on the note list, the user synced and reopened the note and,
saying **“Evet, artık görünüyor”** (user's words: "Yes, it's visible now"), confirmed that the Linux line was seen on device.
The controlled plain-text round trip was thus completed on a real device.
(The fix, repair/readback and device confirmation records are kept private.)

This is not evidence of general-purpose note editing or atomic conflict
resolution: there is a race window between compare and update.

Validation at this stage: 65 tests in the Rust workspace (including doctests) and 79 tests in the Python
research tools passed; Rust format and Clippy checks clean.
Live requests were not counted among the tests. Gaps at that stage: full inventory and
persistent cursor, session renewal, offline queue, deletions/conflicts, attachments and
drawings, a lossless general editor, the Tauri UI.

The live write path of the old single-page rebuilder `tools/notes_update.py`
was disabled; its helpers and synthetic historical tests are kept. The live-verified
path is in `tools/notes_snapshot_update.py`.

The sections below preserve the scope and evidence of earlier stages.

## Source and scope

Android Notes 4.4.45.37 APK SHA-256:
`daed1eff8c8ee9dfb8afe2771e39e893a8808f3230d6d522a8aa647db09b8667`.
The change reader in the Android app (`b4.a`) reads `NoteChangesView`, `hasNext`, `lastChangePoint` and the note fields.
`networkutils.l.d()` selects HTTP GET. The relevant endpoint is
`/samsungnotes/v1/notes/changes` or `/samsungnotes/v3/notes/changes`, with the query
field `lastChangePoint`. Server/account negotiation of which version to use is not yet
resolved; the client does not pick one on its own and send a request.

`notes-protocol` preserves cursor and commit values without modification; it keeps a missing
deleted flag as `None` instead of turning it into false. Raw
XML fragments of unknown elements are preserved in input order. These fragments are not independent,
re-sendable XML documents: they may depend on the namespace declarations at the
root. The parser makes no claim of full XML roundtrip or of being a lossless note
editor. The note-level `isEncrypted` and EDP's `encrypted` attribute on noteTree
will not be treated as the same concept.

The 2 MiB input and 50,000 XML node limits, mandatory pagination/ID fields,
rejection of duplicated known fields and explicit boolean/numeric checks are our
client validation policy. They are not presented as the complete schema of the Samsung
server. DTD entity definitions are not accepted; there is no external entity resolver.
Error messages do not print note IDs or response content.

Following comparison with the `a1.t.Q/R/S/T` helpers, whitespace normalization for booleans and numbers
was removed. For booleans only true/false
(case-insensitive) is accepted; although Android turns other text into false,
our validation policy rejects ambiguous input. Numbers are read as
decimal or as hex with a lowercase `0x` prefix; time is signed 64-bit, the
SDK version signed 32-bit. Android's fallback that drops a corrupt SDK value to zero
is not applied here; an explicit error is returned. These differences were first shown with failing
regression tests and then fixed. In this first stage 15 tests passed;
Clippy `-D warnings` clean. Evidence: the private evidence archive.

The fixture is hand-made synthetic data. Preserving multiple `changes` elements
is tested; the real server has not yet been observed sending multiple notes on one
page. How the Android app populates this field should be compared against a real
response. The current tests do not prove live protocol compatibility.

## Running

```sh
cargo test --workspace --locked
cargo run -p notes-cli --locked -- inspect-changes crates/notes-protocol/tests/fixtures/changes.xml
cargo run -p notes-cli --locked -- inspect-tree crates/notes-protocol/tests/fixtures/tree.xml
```

The CLI only shows counts and pagination state; it does not print note IDs, the cursor or
field contents. It does not modify the input file. A command not yet implemented,
such as `sync`, returns an error.

## Note tree

`tree::parse_note_tree` now parses the `noteTree` and `CompareForUpsyncView` roots,
the page → layer → object hierarchy, content files, floating objects and the
body text record. Source: `n1.o`;
this is not a document renderer or a diff engine. Hash values are preserved
without interpretation. The tree's `ver` and `t` fields are read as decimal in the source;
the hex support from the changes response is not applied to these fields.

The original XML is stored with its namespaces and unknown attributes; there is not yet a serializer
that can modify and re-send it. In addition to the 2 MiB / 50,000 node limit,
64 depth, 4,096 entity and field/extra data limits apply. These are client
policy. The CLI only shows structure counts and does not print the stored XML.
With 11 tree tests and an additional CLI test, **27 tests passed** in the workspace;
Clippy `-D warnings` clean. New evidence: `tests-with-tree.log` and
`clippy-with-tree.log`; synthetic tests do not prove live server compatibility.

## Next evidence

1. Samsung Account user interaction, redirects, session renewal and the
   acceptance conditions for an independent application.
2. Acquiring an SCSP Cloud token separate from the account token, and ERS address selection.
3. Parser comparison against a real, controlled `NoteChangesView` response.
4. The relationship between NoteTree/metadata, binary files, upload URL and commit.
5. Downloading and opening a test note; sending a controlled change back, followed by
   offline editing, deletion and conflict behavior.

A Windows VM was set up; the official Notes recognized the account and started downloading notes.
The running official Notes and Android devices
will be compared with the same test note in a reference session (private, not in the repository).

## Account preparation

An offline primitive was added for [PKCE S256](PKCE.md). 32 bytes of operating system
randomness, strict RFC verifier checking and secret-hiding Debug behavior are tested.
33 tests + 1 doctest passed in total; Clippy/format clean. This module does not provide HTTP or
Samsung sign-in compatibility. The account research (kept private, not in the repository)
separately records the real GET start request, the local package check and the remaining uncertainties.
Protocol work continues without waiting for the test note transfer.

In the next stage the `entrypoint` module and the `inspect-entrypoint FILE.json` command
were added. The request builder implements the GET / countryCode / Basic(appId + empty password)
contract and does not send HTTP; the response parser reads bounded JSON metadata.
The HTTP 200 response obtained from Linux with the Python probe was also
parsed with the Rust CLI. At this stage **42 tests + 1 doctest** passed; Clippy/format were clean.
The metadata parser alone does not validate the sign-in URL or the RSA key.

The subsequent `signin::PreparedSignIn` layer added the fixed Samsung sign-in URL and DER/RSA
checks. It places a new PKCE/state into the 34-field profile; after the KDF/AES/RSA chain verified
against an independent Java fixture, it applies double URL encoding.
`notes-cli prepare-signin ENTRYPOINT.json PROFILE.json NEW_OUTPUT_DIR`
creates the private URL and V2 transaction files offline. IDs in the profile
are not corrected automatically: the Notes IPC ID/callback and the Account
web OAuth inputs differ; details are in the private account research report (not in the repository).

For the correct web profile sent via the Python probe, the server returned HTTP 307;
comparison of the sent state, both client IDs, callback and response_type
succeeded. No sign-in with the user account was performed. `tools/capture_oauth_callback.py`
binds a URI given on stdin to the operation target/state/duration and records a valid return
once to a private file. There is no desktop URI handler registration and no token request.
The current total of **49 Rust tests + 1 doctest and 6 Python callback tests** passed.
Synthetic Rust CLI → Python callback capture → replay rejection was also checked
offline. These tests do not prove real callback delivery or token acceptance.
Clippy `-D warnings` clean.
