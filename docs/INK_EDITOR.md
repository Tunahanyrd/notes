# Ink data write research

> Note: The evidence records referenced here (sanitized measurements, raw analysis outputs) are kept private and are not included in the public repository.

`samsung_notes/ink_editor.py` reads the real stroke channel and frame format;
it does not imitate handwriting editing by drawing on top of a PNG. The first
document producer only prepares an offset copy of an existing supported stroke.
Free-form pen input, a brush engine and general editing of existing drawings
are not enabled yet. This module sends no network requests.

## Verified format

The Cloud part is `strokeResource`, MIME type `application/octet-stream`, file
name `UUID_modifiedTime`. The XML `object(type=stroke,id,modifiedTime,hash)`
carries the same ID/time. The hash is not a content digest but the value
`SHA256(UTF8(UUID + decimal modifiedTime))`.

The outer envelope is `ffffffff`, LE32 header length 4, an LE32 format field,
followed by the native frame chain. Outer format4000 was observed in Android
samples, and outer format0 in the controlled 007 sample created on Windows. In
both, the inner common frame is format4000. The editor codec accepts only these
two outer profiles, with inner-4000 validation, and preserves the original
envelope. The general behavior of the old `ink.py` wrapper was not changed.

The first frame is type0, the common object; the second frame is type1, the
stroke. Each frame consists of length, type, flexible-data offset, variable
property/field masks, fixed data and the flexible fields in mask order. The
common frame contains the UUID, modifiedTime, drawn bounds and additional object
metadata.

Stroke channels are separate arrays:

| Channel | Compressed | Uncompressed |
|---|---|---|
| X/Y | First two f64, then sign-magnitude u16 Q5 delta pairs | Two f64 per point |
| Pressure | First f32, then sign-magnitude u16 Q12 delta | f32 array |
| Time | First i32, then positive u16 delta | i32 array |
| Tilt/orientation | If property bit2 is set, separate f32 seed + Q12 arrays | If bit2 is set, separate f32 arrays |

Property bit0 is tied to both the curve and the compressed representation; it is
not toggled like an ordinary compression option. The last two fixed bytes are
the raw tool type. When points are restored, each step accumulates at float32
precision. The encoder's first subset accepts only Q5/Q12 differences that are
represented exactly; it does no silent quantization. Rewriting the same data
preserves the original bytes. This does not mean we implement the full behavior
of the native lossy reducer.

Style field1 is the advanced-setting string ID, field7 the pen-name string ID,
field2 the ARGB color and field3 the f32 width. The IDs are resolved in the
note's `attributes/stringIdList/stringId(id,text=Base64UTF8)` table. The stroke
part alone is not a complete brush definition. The other known flexible fields
are read with their types; an unknown bit, legacy partial-rectangle data or an
unresolvable string ID is refused (fail closed). Flexible bytes are preserved in their
original form instead of being rebuilt.

## First narrow document preparation contract

```python
prepare_ink_clone(snapshot, source_stroke_id,
                 new_stroke_id=stable_uuid, dx=32, dy=32, now_ms=wall_clock_ms)
```

`InkAppendEdit` returns the full expected XML, the full noteTree, the filtered
upload XML, the page/layer/new-object ID set and the new binary part. The Cloud
adapter must apply a fresh commit/snapshot check, the full typed compare result,
a persistent single-send ID and a post-publish XML+binary readback check.

The initial profile requires format4000, ungrouped normal FountainPen/InkPen2,
an empty canvas cache, thumbnail=-1, and zero attachments/content-files and
floating objects. Eraser, replay/effect, lock, encryption, an unexpected
metadata/part inventory or drawn bounds extending beyond the page are rejected.

The clone shifts the first XY seed and the common drawn bounds by the same
amount and renews the UUID and tick. All delta bytes, pressure, time, tilt,
orientation and brush metadata are preserved. If float32 accumulation drifts
from the offset target, the operation is rejected. This way no brush-width-
dependent drawn-bounds calculation is invented for an arbitrary new stroke.

The new XML object and tree entry are added; layer and page hashes are
recomputed from the ordered child hashes. The original hashes are verified
first as well. The page drawnRect union grows. Root sync/displayModifiedTime
advances in ms, and the content tick advances in a separate raw integer field.
Other pages, the body, title, string table and existing strokes are preserved.
The upload carries only the requested page/layer/new-object delta and the new
MIME stroke.

**Documents that have a cache are rejected.** Until the cache
deletion/regeneration contract is proven, it is not correct to leave the old
image in place or to blindly delete all cache fields. In the controlled 007
sample, two strokes, two pages, zero canvas-cache and zero content-file were
seen; the first attempt fits this existing narrow profile.

## Verification and limits

- 11 synthetic codec/append tests: an independently hand-built golden channel
  array, non-UTF binary framing, bounds, quantization rejection, metadata/part
  preservation, timestamp separation, source ID and cache rejection.
- 12 strokes in the existing private cache were decoded; unchanged byte
  roundtrip and the native UUID/time/hash + XML/string-table binding were
  verified 12/12. Private geometry, IDs or text were not brought into this
  document/the repo.
- The real `sm_RestoreStroke(0x2ed234)` instructions in the hashed
  `libSPenModel.so` from the user's APK verified the encoder output in six
  synthetic samples, against the float/int bytes of all five channels.
- The real `NewApplyBinary(0x2ee650)` type1 reader verified channel, color,
  width, pen/advanced-setting ID and tool values in 12 synthetic cases for
  compressed/raw × stylus on/off × 1/3/32 points. Memory was pre-allocated and
  the native implementation fields were supplied explicitly. The object
  constructor, the type0 reader, the string manager and brush rendering were not
  executed. These are not proof of the full application lifecycle.
- Full offline preparation with dx32/dy32 on the controlled 007 snapshot
  succeeded. The researcher of this module did not write to Cloud; a new stroke
  visible on the device is not yet this checkpoint's success criterion.

The native experiment ran only on synthetic data, with a single CPU, 256MiB of
address space, 15 CPU seconds, and a limit of 100,000 instructions/2s per call.
Host callbacks are limited allocation/delete/memcpy/memset and buffer-bounds
checks; decoder/property/style operations are native instructions. In the first
harness attempt, a stack-copy rejection and missing wrapper context were fixed;
only completed controlled calls were counted as successes.

The existing SPI preview provider is the `libSPenBase` raster codec; it cannot
be used in place of a stroke writer. A full local Spen writer would additionally
need the object/context, string manager, constructor, size/write and brush cache
lifecycle. A standalone frame encoder is possible; native serialization/render
equality, general brush bounds, a free-stroke quantization policy and cache
updates await separate verification.

Source hashes/addresses and evidence separation:
the private evidence archive.

## Free-form point input: second offline stage

`samsung_notes/ink_capture.py` provides two helpers without changing the
existing strict codec/clone behavior: `normalize_capture` explicitly returns the
native compression loss; `basic_pen_drawn_bounds` computes the native bounds
only for the unrotated FountainPen/InkPen2 profile. `prepare_ink_append(snapshot,
source_stroke_id, new_stroke_id=..., samples=StrokeSamples(...), now_ms=...)`
produces a full `InkAppendEdit` candidate that passes the same cacheless
document guards. The source stroke is the template for the existing
brush/style/string table; the old stroke is not modified. It is not yet wired to
the Cloud adapter or to the app's editing capability.

When native `SetPenType(0x2ea3c0)` does not find these two pen names in its
special enum branches, it selects enum2. `GetRectByPoint(0x2e9258)` returns the
float32 min/max box of the points. `GetDrawnRect(0x2e9488)` expands it by
`f32(f32(size * 0.5) + 4)` in every direction for enum2. Other pens have
different multipliers and margins; this formula is not used as a general pen
bound. Rotation, group/container and special effect profiles are outside this
stage.

Normalization was compared against native `sm_ReduceStroke(0x2ec1a8)`:

- XY inputs become float32. The first values are written as f64 seeds.
  Subsequent points are differenced against the previous **reconstructed**
  float32 point and rounded to the nearest 1/32 step; at exactly half a step
  they round away from zero.
- Pressure/tilt/orientation become float32. Q12 differences are computed from
  the previous **original** channel value and truncated toward zero. The decoder
  accumulates these differences; rounding the absolute values one by one is not
  equivalent. The returned result contains both the canonical samples and the
  measured XY/pressure loss.
- Times are integer ms. The Android adapter/recording source trace shows that
  they are event time minus down time. This client's capture subset requires the
  first time to be zero; this is not a general requirement of the native format.
  Negative/out-of-order intervals or intervals exceeding 65535 ms are not
  silently clamped.
- The first document producer does not request stylus-axis channels. The
  browser's tiltX/tiltY fields cannot be written directly in place of native
  tilt/orientation. The existing template's other property bits, tool type,
  advanced settings and brush bytes are preserved; axis-presence bit2 is cleared,
  consistent with the two new axis-less channel arrays.
- The new native bounds are computed from the canonical points. This candidate
  does not claim to be equivalent to the whole flow in which the native app
  filters/smooths raw motion and saves it; it only writes the canonical geometry
  at hand.

The proposed UI draft contract is `schemaVersion:1`, the existing
page/brush-source ID and a `samples:[{x,y,pressure,timeMs}]` array. X/Y are
document coordinates, not CSS pixels or screen pixels. The first capture path
accepts only orientation=0 pages; other transforms must be verified
separately. Pressure is 0..1, time is integer ms from the start of the down event; tilt fields are not accepted in the
first stage. A persistent job must generate the new UUID once and freeze the
normalized result and the source commit; rerunning it must not produce a new
Cloud POST. There is no HTTP edit endpoint yet.

**Verification:** In 8 synthetic native reducer cases the produced channels are
byte-for-byte identical; 48 native bbox comparisons, including two pens × three
widths, match exactly. The real `GetRectByPoint`, `GetDrawnRect`, `SetPenType`
and Base `RectF` instructions were executed. The string comparison, refresh flag
and zero-rotation context are harness stubs; they are not the renderer or
constructor lifecycle. The new capture/helper tests and the existing
clone/Cloud-clone tests passed together 30/30. An offline candidate with three
synthetic points could be prepared on the controlled 007 snapshot; it was not
sent to the cloud, and personal point geometry was not brought into the repo.

**Cache limit unchanged:** native `WForegroundHandler::ClearCanvasCacheData`
(0xcccd4) does not just empty the map; it calls a media manager virtual method
and updates the related native state flags. The separate
`ClearCanvasCacheDataMap(0xcd738)` is a narrower operation. `n1.u` skips the XML
element if the map is empty; this alone does not prove a contract for safely
deleting/renewing the old media/contentFile and thumbnail relationships in
Cloud. Cached documents are still rejected.

Second-stage evidence: the private evidence archive.

## Live ink preview (2026-10-27)

The drawn stroke is no longer a fixed-width SVG `polyline`: `crates/notes-ink-live`
(GPL-3.0, pinned sdocx + resvg 0.49, WebAssembly) imitates the record the writer
will store (rs-ink Q5/Q12 quantization, template pen + recipe 3, recipe 4 on an
empty page) and draws it at screen scale through the page renderer's path. The
UI runs the module in a Web Worker; the latest unprocessed samples and the
predicted tip (`getPredictedEvents`, never stored) are shown as a straight line
for that frame and replaced when the full raster arrives. When the pen lifts,
the on-screen pixels move to the stroke's own canvas in the same frame; the
draft and the queued stroke use the same raster. A pressure-0 sample arriving on
pen lift takes the last contact pressure, and the lift sample at the same
position is not stored (a zero-radius stamp made sdocx reject the whole Fountain
stroke).

The template pen comes from `GET /api/notes/:id/ink-pens?revision=N`:
`{schemaVersion:1,revision,pens:{<strokeId>:{width,height,rendering}}}`;
`rendering` is byte-for-byte identical to the object the page renderer takes for
that stroke (notes-preview `pen_templates`, contains no sample geometry). If the
endpoint is missing, the pen template is unknown or the `.wasm` is missing, the
old SVG preview silently stays. Highlighter, eraser and lasso are unchanged.
Build: `tools/build_ink_wasm.sh` (details: crates/notes-ink-live/README.md).
Measurement: `node tools/ink_live_fidelity.mjs`.
