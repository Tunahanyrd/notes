# Fuzzing

`fuzz/` is a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer)
harness for every parser that consumes data a broken or hostile Samsung Cloud
response, a hostile local client, or a tampered cache could control. It is a
separate cargo workspace (nightly toolchain, own `Cargo.lock`), excluded from
the main workspace, so stable builds and `cargo test --workspace` never see it.

The targets call the real public entry points of the product crates and only
check that they return — `Ok` or a proper error — without panicking, hanging,
overflowing the stack or allocating without bound. Nothing opens a socket,
talks to Samsung or reads a library: the local API and the HTTP client are
driven through in-memory streams, and the seed corpora are built from the
committed synthetic fixtures only.

## Targets

| Target | Entry points | Input |
|---|---|---|
| `payload` | `rs_doc::mime` (headers, parts), `rs_doc::payload::parse`, `rs_ink` inventory and part reader, `notes_preview` payload / ink inventory / `inspect_pdf_backgrounds` (parser only, no Poppler), `notes_cloud::api::decode_parts` (gzip + multipart) | `<content-type>\n<body>` |
| `document` | `parse_document` of a downsync body, then the editor, ink, page-geometry, page-style, page-ops, shape, template and organization projections, `stroke_inventory`, and the renderer page preparation (`notes_preview::ink::prepare`) | `<content-type>\n<body>` |
| `xml` | the ElementTree-style readers of rs-doc, rs-editor, rs-ops, notes-cloud, the Cloud metadata readers (folder changes, note list), notes-protocol change feed and note tree, `pdf_layout`, the shape object reader | XML bytes |
| `cloud_response` | the compare guards (body text, ink, page, new pages, page ops), the create response view, the createUploadURLs scan, the change-feed reader, `json::loads`, `gunzip_limited` | selector byte + body |
| `ink_binary` | stroke resource wrapper, frame chain, point channels (all profiles), `decode_stroke_resource`, channel re-encode / replace, `inspect_spi` | selector byte + binary |
| `json` | the four JSON readers (`rs_store::val`, `notes_cloud::json`, `rs_engine::pyjson` strict and lenient, `notes_login::json`) and their serializers, `EntryPointMetadata::parse` | JSON bytes |
| `api_values` | local API bodies decoded like the server (`pyjson` → `from_json`) and handed to the pure validators: preferences, shortcuts, editor model, ink points, shape intents, text fields, organization, snapshot projections | selector byte + JSON |
| `http_response` | notes-net response parsing over an in-memory stream (`notes_net::fuzz_exchange`, behind the `fuzzing` feature): status line, interim responses, headers, Content-Length / chunked bodies and trailers, redirect `Location`, gzip Content-Encoding with the decoded bound | selector byte (method, recv size) + response |
| `login` | callback URI validation, strict query parsing, unquoting, encrypted callback fields and state, the engine / Cloud / preview URL splitters, desktop-entry quoting | selector byte + UTF-8 |
| `local_api` | `pyhttp::read_request` + `server_core::dispatch` on a cursor (request line, headers, Host/Origin/token checks, routing, path and query parameters, bounded JSON and PDF bodies, static files) with a stub backend that runs the pure validators | selector byte + request (bit 0: the harness prepends a valid request line suffix, Host and token) |
| `png` | `rs_render::pixels` PNG validator / decoder / unfilter, PPM reader | two size bytes + image |
| `spi` | the SPI canvas-cache decoder (`notes_preview::spi`) | SPI file |

## Running locally

cargo-fuzz needs a nightly toolchain. Nothing has to change in your default
toolchain; an isolated rustup works:

```sh
export RUSTUP_HOME=$PWD/.fuzz-rustup CARGO_HOME=$PWD/.fuzz-cargo PATH=$PWD/.fuzz-cargo/bin:$PATH
curl --proto '=https' -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain nightly
cargo install --locked cargo-fuzz --version 0.13.2
```

(or simply `rustup toolchain install nightly` and prefix the commands below
with `+nightly`). Then, from the repository root:

```sh
python3 fuzz/make_seeds.py                         # fuzz/corpus/<target>/ from synthetic fixtures
cargo fuzz build --sanitizer none                  # all targets
cargo fuzz run --sanitizer none xml fuzz/corpus/xml -- \
    -max_total_time=900 -max_len=65536 -timeout=10 -rss_limit_mb=2048 -malloc_limit_mb=2048
```

`--sanitizer none` is the fast mode for long local runs: the parsers contain
no `unsafe` code, and AddressSanitizer made the document targets about seven
times slower. Panics, timeouts and the RSS/malloc limits are detected either
way. A stack overflow is not: without a sanitizer libFuzzer's SIGSEGV handler
runs on the exhausted stack, the process dies with exit status 139 and no
artifact is written. If a `--sanitizer none` run ends like that, replay the
corpus with the default ASan build (`cargo fuzz run <target> fuzz/corpus/<target>
-- -runs=0`), which reports the overflow and saves the input. CI always uses
the ASan build, as should any change touching `unsafe` code or a C dependency
(zlib through flate2).

`cargo fuzz list` shows the targets. Crashes land in `fuzz/artifacts/<target>/`;
reproduce one with `cargo fuzz run --sanitizer none <target> <file>` and
minimise it with `cargo fuzz tmin --sanitizer none <target> <file>`.
`fuzz/corpus/` and `fuzz/artifacts/` are not committed.

Do not feed real notes, sessions or library files to the fuzzer, and never
commit a corpus derived from them: every committed input must be synthetic.

## Fixing a finding

1. Minimise the input and keep it under `fuzz/regressions/<target>/` (a short
   descriptive file name; synthetic data only). `make_seeds.py` adds these
   files to the corpus and CI replays them first.
2. Add a unit test in the owning crate that fails (panics / overflows) before
   the fix and passes after it.
3. Fix by returning the crate's proper error. Keep the existing Turkish
   messages and error codes, and do not change behaviour on valid input: the
   golden conformance (`cargo test -p notes-engine --test golden_conformance
   -- --ignored`, or `crates/notes-engine/conformance/run.py --replay`) must
   stay green.
4. Prefer explicit bounds where allocation or recursion depends on input
   (nesting depth, counts read from headers, decompression output).

## Regression inputs

| File | Bug |
|---|---|
| `regressions/xml/rs-ops-deep-nesting-20000.xml` | `rs_ops::xml::parse_str` converted and dropped the tree recursively; 20 000 nested elements overflowed the stack and aborted the engine. Fixed with the same iterative 256-level bound the other ElementTree readers use. |

## CI

`.github/workflows/fuzz.yml` runs weekly and on demand (`workflow_dispatch`,
optional seconds per target): one job per target builds the seeds, replays
`fuzz/regressions/<target>/`, fuzzes the AddressSanitizer build for the given
time (120 s by default) and uploads `fuzz/artifacts/<target>/` when it finds something.
