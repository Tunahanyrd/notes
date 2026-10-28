# Samsung Notes structured text editor contract

> Not: Buradaki kanıt kayıtları (arındırılmış ölçümler, ham analiz çıktıları) özel tutulur ve herkese açık depoda yer almaz.

This is a conservative implementation of the Notes 4.4.45.37 format-4000
text-object annotation stream. It is not HTML, Markdown, or a complete Spen
document writer. Source references and digests are in
özel kanıt arşivi.

## Product/API boundary

`editor_projection(snapshot)` returns `schemaVersion: 1`, `capabilities`,
`model`, and `reason`. `titleEdit` is independent of `textEdit`/`richStyle`:
an ink/PDF document can have a supported title even when its body cannot be
edited. `inkEdit` remains false. Unsupported body annotations are never
silently converted to plain text.

HTTP integration uses `GET/PUT /api/notes/:id/editor` and
`POST /api/notes/editor`. The application adds its local `revision` to the
projection; PUT requires `expectedRevision`. The Cloud methods are:

```python
CloudAPI.create_editor(title, model, *, operation_id, local_id=None)
CloudAPI.update_editor(server_id, snapshot, title, model,
                       baseline_commit=None, *, operation_id)
```

Successful results use the existing `{server_id, local_id, commit_id,
snapshot, verified}` contract. Operations retain an immutable model and
stable generated draft. A potentially published operation is reconciled by
reading; it is not automatically published a second time.

## Model version 1

```json
{
  "schemaVersion": 1,
  "text": "First\nSecond",
  "spans": [
    {"start": 0, "end": 5, "kind": "bold", "value": true, "interval": 3}
  ],
  "paragraphs": [
    {"index": 0, "alignment": "left"},
    {"index": 1, "bullet": {"kind": "check", "checked": false}}
  ]
}
```

Span offsets are UTF-16 code units, end exclusive. A boundary inside a
surrogate pair is rejected. Supported kinds are `bold`, `italic`,
`underline`, `strikethrough`, `fontSize`, `foregroundColor`,
`backgroundColor`, and `hyperText`. Boolean false is meaningful and retained. Colors are
canonical `#AARRGGBB`, serialized as native signed 32-bit ARGB integers.
Font sizes are native positive floats; a Windows picker-to-CSS-pixel scale
has **not** been established and must not be invented by the renderer.

Native interval values are 0 inclusive/exclusive, 1 inclusive/inclusive,
2 exclusive/exclusive, and 3 exclusive/inclusive (SDK default). Interval,
span order, adjacent boundaries, and zero-length spans are retained. A UI
that cannot preserve those distinctions must decline that projection.
Same-kind overlapping nonempty spans are currently rejected.

Paragraphs have an index and optional `alignment` (`left`, `right`,
`center`, `justify`), boolean `parsingState`, `indentLevel` with numeric
`level` and `direction` (`none`, `ltr`, `rtl`), and `bullet`:

- `check` (native type 2) with boolean `checked` — the to-do checkbox.
- Every other kind has a non-negative integer `number` (default 0): `none` 0,
  `number` 4, `alphabet` 6, `roman` 7, `disc` 8, `whiteCircle` 9,
  `upperAlphabet` 10, `blackSquare` 11, `whiteSquare` 12. For the numbering
  kinds (4, 6, 7, 10 — `s1.h.l()`) it is the current sequence number.
- Optional `typeList` retains supported native bullet-type integers.

Wire value (`s1.h.j/k`): `type,second[,{t1,t2,...}]`, where `second` is
`true`/`false` for CHECKER and an integer otherwise; the `{...}` list is only
written when non-empty. `initialNumber` exists in the binary SDK record but is
not part of this text serialization. Only the types accepted by
`SpenBulletParagraph.isSupportedBulletType` (0, 2, 4, 6–12) are modelled;
ARROW 1, DIAMOND 3, CIRCLED_DIGIT 5 and unknown values keep the body
read-only. The toolbar creates only `disc`, `number` and `check`; the other
kinds are preserved and drawn with an approximate marker (native glyph,
punctuation and indentation not verified).

Font size is `span/fontSize` (type 3), `interval,Float.toString(size)`;
an integer spelling from another client (`15`) is read and re-emitted as
`15.0`, also in `spanOrderData`. Highlight is `span/backgroundColor`
(type 17), `interval,Integer.toString(signed ARGB)` (`s1.f`). The 2026-10-26
read-only survey of the private library copy (names/value kinds only) found
`fontSize` (mostly 17.0, one integer `15`) and `foregroundColor`, but no
`bullet` or `backgroundColor`; list/highlight support therefore rests on the
APK source and upstream `sdocx` `BulletType`, not on observed notes.

`parsingState` is a boolean, not an integer. It is read with
`Boolean.parseBoolean` semantics (`s1.s`): `true`/`false` in any case
(another client stores `False`); other strings are rejected; it is always
re-emitted lowercase. CR and LF are distinct native paragraph separators.
Native subsequent paragraph ranges include the preceding newline; this is
handled by the codec. On read, a paragraph annotation is mapped to its
paragraph by its START only, as `q1.n` does (start→index map; the stored end
is unused natively); a start that is not a paragraph start is rejected. An
identical duplicate for the same paragraph (seen: a zero-length copy of
`parsingState`) is dropped; differing duplicates are rejected. Re-encoding
writes canonical one-paragraph ranges. Styled leading/empty paragraphs,
unknown paragraph properties, font-name spans, embedded object spans, and
unknown annotation grammar currently reject editing.

### Links (`span/hyperText`, type 9)

Value (`s1.o`): `interval,type,dateTimeType[,data]`. `type` is
`WDocHyperTextSpan$Type` 0–9 (1 e-mail, 2 phone, 3 URL, ...),
`dateTimeType` is `$DateTimeType` 0–21. Model value:
`{"type":3,"dateTimeType":0,"data":null}`; `data: null` means the field is
absent (stored notes write `2,3,0` with no data field, sidecar
`9,start,end,2,3,0`), `""` means present but empty (Android `l()` shape).
`data` containing `,` (truncated by native `m()`), `'`, control characters
or `<?`/`?>` is rejected. The UI shows the span as a link (non-inclusive
mark, so typing after it does not extend it), the text stays editable and
the stored value is carried unchanged. No link is created and none is
opened from the editor.

### Page slices (`sectionData/sectionData`, `s1.c`)

Value: the slice's UTF-16 length. These are the per-page text slices of a
page-list note (`SpenObjectTextBox.get/setTextSectionData`; upstream
`RichTextSection`, "a page's slice of a document-level flowing text object";
`TextOnlyNoteManager` re-measures them with `measureText`). They are
layout data, not formatting, and are kept OUT of the editor model, like
`spanOrderData`. Read (`db.n.t`): a close belongs to the open slice whose
start+value (start, for value 0) equals the close position, otherwise to the
latest open slice; the real range is always open→close. Slices must be
ordered, non-overlapping and on UTF-16 boundaries, else the body is
read-only. Stored notes end with a separate empty-slice instruction; the
encoder emits slices after spans/paragraphs and empty slices in their own
instruction, and proves by re-reading that the stream yields exactly the
intended slices (else the save fails closed).

On a body save the slices follow the text change (`remap_sections`): the
change is the region between the common prefix and suffix; boundaries
before/after it stay/shift, a boundary inside it is clamped into the
replacement, a boundary at the end of the text stays at the end. Unchanged
slices keep their raw value byte for byte; a slice whose range changes is
written with its real length (the value `db.n.t` derives anyway). Native
re-measurement after the edit is assumed, not verified on a device. A title
containing slices is not editable. An unchanged body is never re-encoded.

An untitled note may have no title `<text>`; body editing is still allowed
(the title object is not touched) but title editing stays disabled.

## Wire grammar and preservation

The XML `text` value is Base64 of a stream containing Base64 text chunks and
annotation instructions such as `<?a 'span/bold'='3,true'?>` and closing
`<?a 'span/bold'?>`. Native serializer positions use inclusive ends while
SDK/model offsets use exclusive ends. `spanOrderDataList` and
`zeroLengthSpanList` records use the `val` attribute containing Base64 of
`type,start,end,interval,value`.

Body editing is enabled for the bounded format-4000 title/body text
objects. Since editor rules v2 (`editor-projection-v2`, 2026-10-28) the
page content next to them no longer blocks it: layer objects (handwriting,
images, shapes), the content-file lists, the attributes' string table and
binary MIME parts are carried unchanged. An editor upload sends the note
tree and the attributes, the changed body object, and an EMPTY page list
and content-file list — the same recipe as the Cloud-verified title edit on
pen notes — so no page object or binary part is re-sent or rewritten.
Every page/layer and unknown permitted metadata value is preserved.

A native empty text box has no `<text>` child (every handwriting-only and
untitled device note in the 2026-10-28 survey). It reads as empty text; a
first edit appends `<text>` as the last child (the native order is
`rect, lineColorEffect, lineStyleEffect, originalRect, [sidecars], text`);
clearing a title/body through an edit writes it back without `<text>`
(new notes keep the research writer's `<text/>`). Page slices keep a cleared
body's `<text>` (zero-length `sectionData`).

Still read-only, with plain-language reasons: free-floating objects
(`floatingObjectList` non-empty), voice/attachment/extra-bundle nodes,
objects outside the page list, page objects whose ids disagree with the note
tree's layer inventory, unknown annotation grammar, locked/encrypted notes,
and more than 32768 annotations per text object (was 4096; long device notes
carry one `parsingState` per paragraph). Cached projections carry no rule
version, so the store drops synced notes' cached projections (and refused
title flags) once per rules change (`metadata.editor_projection_recipe`).

Re-encoding is canonical, not byte-preserving: on the private library copy
(55 notes) a no-edit re-encode was byte-identical for 39 and
document-equivalent (same model and page slices) for all 55; the differences
are native paragraph-close positions, dropped identical `parsingState`
duplicates and `15`→`15.0`. An unchanged body is never re-encoded.
Measured with `cargo run --release -p notes-host --example editor_survey --
<private copy> [--roundtrip]` (structural output only).
Title-only changes preserve the full original body/page/content-file data;
the upload contains the full unchanged note tree and changed attributes,
without unchanged body/page data chunks.

Updates fetch a fresh baseline and compare resource identity, commit,
original XML, and opaque part fingerprints. `compareForUpsync` must request
exactly the changed body object (or no object for title-only edits). The
root sync and display times advance in wall-clock milliseconds; internal
content ticks advance independently of their observed unit domain.

Readback must match the title, body, normalized rich model, expected full
XML fingerprint, and opaque binary fingerprints where applicable. The
expected body is the text extracted from the draft itself (the same
extraction the readback snapshot uses, which also includes text of page
objects), not just the model text. The
editor fingerprint version 2 additionally treats absent and empty root
`contentFileList`/`floatingObjectList` and layer `objectList` as equivalent:
Cloud materializes those containers on downsync. Nonempty containers,
unknown attributes and namespaces remain significant. The original title
fingerprint version 1 stays unchanged. Legacy editor receipts are first
validated against their original draft hash, then compared using the narrow
version 2 equivalence; saved receipts are not rewritten. This is a guarded
write, not an atomic server-side CAS.

## Evidence and remaining verification

The implementation has source-derived golden grammar tests, UTF-16/color
tests, preservation tests, and mocked Cloud durability/conflict/readback
tests. The combined document/Cloud suite passed 63 tests on 2026-10-25.
Offline projection of three existing large cached ink documents enabled
title editing and correctly kept body editing disabled. No personal note
was modified for this investigation.

The root agent created one synthetic rich note through the application:
Cloud returned 201. Initial readback verification correctly stopped on the
previously unknown empty-container materialization. After that specific
representation difference was checked, read-only reconciliation verified
the same published note, exact rich model and preserved content; no second
create was sent. Rich update and visual Android/Windows rendering remain
unverified at this checkpoint. Cloud XML roundtrip alone does not prove
native typography, list rendering, or font-size equivalence. Ink preview
decoding does not establish editable vector ink; brush/pressure/tilt/native
stroke-writer compatibility remains separate work.
