# rs-doc: porting notes

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

Ports `samsung_notes/document.py`, `tools/notes_payload.py` and `tools/notes_writer.py`.
Status: every public name is implemented in Rust. Scoreboard: document 15/15, notes_payload 10/10,
notes_writer 8/8, all STRICT. PYFALLBACK count: 0.

## Layout

The pure-Rust core has no PyO3 types, so the Tauri app can link it directly:

| module | contents |
|---|---|
| `xml` | ElementTree-compatible tree. `parse` uses roxmltree with ET semantics: `{uri}local` names, comments and PIs dropped (text merged), text/tail, and end-of-line normalisation before parsing like expat. `tostring` matches `ET.tostring(..., 'utf-8', xml_declaration=True)` byte for byte, including ns0.. prefixes, escaping and `<x />`. `canonicalize_subtree` matches `ET.canonicalize(ET.tostring(e))`. Parsing and serialization use explicit stacks, not recursion. |
| `mime` | `_headers` / `_content_type` / `_parts`: email-policy header semantics, the Samsung fast path and the boundary marker regex. |
| `payload` | `parse_payload` (`PayloadResult` keeps the parsed roots), `_xml` (`parse_xml`), `decode_rich_text`, `_url` (`url_ok`) |
| `writer` | `ResearchNote`, `object_hash`, `parent_hash`, `encode_plain_text`, `create_multipart` |
| `doc` | `parse_document(_fields)`, `plain_structure` (returns index paths), `prepare_edit`, `new_document`, `multipart`, `page_inventory`, `integer`, `normalize_commit`, ... |
| `util` | strict base64 (binascii strict_mode rules), uuid4/uuid5/`uuid.UUID` parse rules, `gzip.compress(mtime=0)` byte-exact through system zlib (flate2 "zlib" feature), `json.dumps` writer |
| `pyapi` | binding helpers. Other crates can use: `document_error` / `payload_error` (these import the live classes), `element_from_py`, `py_xml_bytes`, `payload_limits`, `doc_limits` |

`document.rs`, `notes_payload.rs` and `notes_writer.rs` are thin bindings.

## Python compatibility details

- **Value classes.** `PayloadPart`, `DownloadUrl`, `ContentFile`, `TextPreview`, `PayloadResult`, `DocumentSnapshot`, `DocumentEdit` and `ResearchNote` are frozen pyclasses (subclassable). They have dataclass equality and hashing, the original reprs, and a real `__dataclass_fields__` table, so `dataclasses.replace`, `asdict` and `fields` work.
- **Exceptions.** `PayloadError` and `DocumentError` are `ValueError` subclasses with `__module__` set to the original module, and `PageOpsError(DocumentError)` still works. Turkish messages are copied verbatim.
- **Patchable internals.** Tests patch some module-level names, so these private names are exported too: `_parts`, `_headers`, `_SAMSUNG_PART_HEADERS` (a real `re.Pattern`) and `_private_write`. At call time the Rust code checks whether the module attribute is still its own builtin. If it has been patched, the code calls the patched object through Python, so call counts and `side_effect` still work. Limits (`MAX_*`) are read from the module on every call, so patching them works too.
- **Other modules' ET elements.** `local`, `one` and `xml_bytes` accept ElementTree elements from other modules. `xml_bytes` converts them to the Rust tree and serializes in Rust. Anything the Rust tree doesn't model (comments, QName objects, non-str values) falls back to stdlib `ET.tostring`.
- **Tools tests.** `tools/test_notes_*.py` import `notes_payload` / `notes_writer` as top-level modules. `_rust.py` does not map those names, so the tools suite still tests the Python code. I checked the Rust implementation against those tests with a runner that aliases `sys.modules['notes_payload'] = tools.notes_payload` (and the same for `notes_writer`). 52/52 pass under rust-strict. Suggestion for the lead: also map `notes_payload` and `notes_writer` in `_rust.py` AREAS.

## Known divergences (all fail closed, i.e. reject)

- **MIME headers.** RFC 2231 parameters (`name*=`), encoded words inside quoted strings, and comments that contain quoted pairs are rejected with `malformed MIME header value`. Python accepts some of these. Everything else matched exactly in differential fuzzing: about 15k random header sets, with the same accept/reject decision, error message and part metadata.
- **Download URLs.** Netlocs with non-ASCII characters are rejected, because the NFKC check is not modelled. IPv6 zone ids are rejected.
- **Case folding.** `casefold()` is approximated as lowercase plus the common special foldings (ß, final sigma, ligatures, ...).
- **UUID parsing.** In `uuid.UUID()` parse emulation, non-ASCII Unicode digits are treated as invalid.
- **Errors from `prepare_edit`.** On a hand-modified snapshot, non-`DocumentError` exceptions from `_plain_structure` are raised as `ValueError` with a Python-like message. `binascii.Error` and `UnicodeDecodeError` are not reproduced exactly.
- **XML error precedence.** When a document is both malformed and over the depth/node limits, a lightweight scan up to the parse-error offset decides which error wins.
- **XML parser.** expat and roxmltree may differ on exotic well-formedness edge cases.

## Verification

- `NOTES_IMPL=rust-strict` passes `tests.test_document`, `tests.test_preview_performance` and `tests.test_editor_document`. The tools tests also pass under rust-strict (via the alias runner).
- The full suites pass under `NOTES_IMPL=rust`: tests 387/387, tools 79/79.
- Differential run: 2000 random documents (mutated attributes, text, namespaces, comments, CDATA, truncation, several MIME header forms). Compared outputs: `to_dict`, `page_inventory`, `prepare_edit` bytes, `parse_payload` metadata and `xml_bytes`. Python and Rust are identical on all of them.

## Performance (synthetic note: 3000 stroke objects, 3000 binary parts)

| function | Python | Rust |
|---|---|---|
| `parse_document` | 130 ms | 8.9 ms |
| `parse_payload` | 78 ms | 7.9 ms |
| `xml_bytes` (Python element) | 19.5 ms | 6.0 ms |

## Notes for other crates

- Each cdylib has its own type objects. Do not instantiate rs-doc pyclasses or the static exception types from another extension module. Use the pure-Rust API, or go through `samsung_notes.document.*` / `tools.notes_payload.*` for Python-visible objects and exceptions.
- rs-editor (`etree.rs`) and rs-ink have their own ET-compatible trees. `rs_doc::xml` is meant to become the shared one. It has the same shape (`tag`/`attrs`/`text`/`tail`/`children`), and the add-only API promise in `STABLE` applies.
