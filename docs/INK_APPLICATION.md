# Ink editing and send queue

`GET /api/notes/:id/ink` adds `revision` and `pendingStrokes` to the
`project_ink` result, which is bound to the note's local revision. Raw binary,
media URLs and account information are not exposed to the UI.
`pages[].strokes` are the source pen styles. For unsupported notes or notes
waiting for sync, `capabilities.appendInk=false`. A pending batch created by
this application does not prevent capturing additional local draft strokes;
those strokes wait for the current batch to be verified before being queued.

`PUT /api/notes/:id/ink` body:

```json
{
  "expectedRevision": 2,
  "strokes": [{
    "pageId": "source-page-uuid",
    "sourceStrokeId": "source-stroke-uuid-on-this-page",
    "points": [
      {"x": 100, "y": 100, "pressure": 0.5, "timeMs": 0},
      {"x": 110, "y": 110, "pressure": 0.6, "timeMs": 10}
    ]
  }]
}
```

The desktop client also supplies `draftStrokeIds`, in the same order as
`strokes`. The store checks each id and its content against the device draft,
then inserts the immutable job and removes exactly those ids from the draft in
the same SQLite transaction. A stale or mismatched draft fails without a job.
The legacy request without `draftStrokeIds` retains its earlier behavior.
`POST /api/notes/:id/ink-draft` with `{revision, strokes}` atomically adds new
stroke ids to the device draft without replacing existing strokes. The client
serializes full draft writes and batch handoff per note; lines drawn while that
handoff is in flight can be added independently. When a first stroke on an
empty page enters a job, later draft lines on that page are bound to its durable
stroke id and wait for verification before they can be sent. An ambiguous local
handoff response blocks draft replacement until restart so it cannot put a
possibly queued stroke back into a new batch.

X/Y are in document units; time is integer milliseconds from the first sample.
The pen style comes from the source stroke. A request contains at most 64
strokes and 20,000 points in total. The source and page binding, the
coordinate/time/pressure limits and the acceptance conditions of the real native
document producer are verified before saving. The response returns `{note}`;
this shows that the local persistent save is complete, not Cloud confirmation.

SQLite schema 6 adds the `ink_documents` and `ink_jobs` tables. The save stores
the immutable batch model, the source snapshot, the UUIDs and the revision
increment in a single transaction. No other body/title save is accepted until
the batch finishes. Local draft strokes are kept in the GET response when the
app is reopened.

Each stroke is sent in a separate verified Cloud operation. The operation UUID
is derived as UUIDv5 from the batch UUID and the stroke index. After a
successful readback, the next index and the Cloud snapshot are checkpointed in
the same transaction. If the operation succeeded but the process exits before
the checkpoint, the recorded result for the same UUID is used; no new publish ID
is generated. An ambiguous send stops the whole batch; there is no automatic
retry. Strokes that were already sent are not rolled back.

New batch models pin the native page scale and the limited cache release
contract with `recipeVersion:2`. Earlier models without this field run as v1;
the meaning of an old operation is not changed under the same ID.

Extracting a plain-text copy from a conflicting drawing and reporting success is
blocked. This version does not copy a note with drawings into a new note; on
error, the original batch and points are kept in the private local record.

This infrastructure alone does not mean general pen support or visual
verification on a real device. Gates on the native cache, page scale and
supported pen profiles must be provided separately. The
`rendering.nativeEquivalent=false` geometry DTO is not equivalent to the
original pen rasterizer.

## Ink in notes with images, shapes and text boxes (2026-10-28)

Status: implemented in source, synthetic tests passed
(`crates/notes-host/tests/ink_objects.rs`). **Not written to Cloud, not seen on
device**; user run: `tools/live_ink_objects_probe.py`.

- The rule lives in one place: `rs_ink::core::objects::passthrough_object`
  (`util::passthrough_object` / `util::ink_objects` in rs-ops). `image`,
  `shape`, `line` and `textBox` objects that sit directly in the page layer's
  `objectList` are accepted under these conditions: canonical UUID, `hash ==
  SHA256(id + modifiedTime)`, no other `object` inside (no group/embedded
  object), no `fileId` (cache) anywhere, exactly one
  `fillEffect type="image" imageId=<content file id>` on an image, and no
  `imageId` at all on other types (a shape with an image fill is rejected). Any
  other object keeps ink disabled for the note, as before.
- No writer changes these objects: the element, its `objectList` order, its
  note tree entry and the image's content file entry stay byte-for-byte
  identical. Layer/page digests are recomputed from the children's recorded
  digests (Samsung rule). Only the edited stroke goes into the upload body; the
  note tree stays complete.
- Cache release (`release_page_cache`): the image's content file is treated like
  a PDF: `refCount` must equal the number of image uses, there can be no cache
  mapping, and its metadata does not change. The file digest is verified with
  the recorded spelling of the `isFileAttached` value (`true`/`True`, both
  observed).
- Writers: append recipe 2/3 (recipe 1 keeps the old stroke-only profile), first
  stroke (on a page carrying only objects it is appended after the objects and
  the existing `drawnRect` grows by union; on an empty page the old behavior),
  highlighter, eraser and move. The eraser/move target can only be a stroke; an
  image/shape/text box ID is rejected. The last stroke on a page can be erased as
  long as carried objects remain on the page.
- Recipe IDs did not change: for every previously accepted input the output is
  byte-for-byte identical (synthetic and oracle differentials). Only inputs that
  were rejected before are now accepted; this is the same approach as widening
  the page bound of recipe 2+ in PDF books. The compare guard did not change:
  the server may name only the page + layer + new/deleted/moved stroke (+ the
  released cache); if carried objects are named, publishing stops.
- Projection recipe `ink-projection-v7:carried-objects` (previously cached projections
  that disabled ink are recomputed).
- Still disabled for ink: locked/encrypted notes, grouped strokes (`strokeGroup`),
  audio/attachment files, floating objects that cannot be carried (see below),
  rotated pages, background image/template URI, first stroke across multiple
  layers, locked/hidden layers, nested or cache-bound objects.

## Floating objects (2026-10-28)

Status: implemented in source, synthetic tests passed
(`crates/notes-host/tests/ink_objects.rs`, `floating` section; the new tests
were red on the old code). Preparation **offline on a private local copy** of
the test note the user made on the phone also passed (only results/counts were
printed). **Not written to Cloud, not seen on device**; user run:
`tools/live_ink_objects_probe.py` (LinuxInk-Probe-015).

- An image added on the phone is written not to the page layer but to the
  note's `floatingObjectList`: `object type="image"` + `pageIndex`,
  `layoutType`, `ownerPageWidth/Height`, `rect`, `connectionPointList`,
  `lineColorEffect`, `lineStyleEffect`, `originalRect`, `path/segmentList`,
  `fillEffect type="image"` (`stretchOffset`, `tilingOffset`, `ninePatchRect`).
  In the note tree: `noteTree/floatingObjectList/object id t h`; content file
  `refCount` 1. The body text anchors the image with `objectSpan` (around
  U+FFFC, value `<id>,…`). This text is editable with `editor-projection-v4`
  (the anchor stays in place and cannot be deleted; details in
  docs/EDITOR_FORMAT.md).
- The rule lives in one place: `rs_doc::objects` (`passthrough_object`,
  `floating_object`, `floating_inventory`); rs-ink
  `core::objects::tree_floating`, rs-ops `util::floating_objects` and the text
  editor use the same rule. Acceptance: the same self-containment rule as page
  objects (image/shape/line/text box, `hash == SHA256(id + modifiedTime)`, no
  nested objects, no `fileId`, a single `fillEffect imageId` on an image), plus
  `pageIndex` must be a page of the note and the note tree entries must match in
  order (`id`, `t`, `h`; leaf). Anything else (unknown type, out-of-range/missing
  `pageIndex`, stale tree entry, group) keeps ink disabled for the note, as before.
- Writers (append recipe 2/3, first stroke, highlighter, eraser, move, shape)
  preserve the list, the tree entry and the image's content file byte-for-byte;
  the list never goes into the upload (it already did not). Cache release counts
  the image's file like page images (`refCount` = uses). The timestamp also
  exceeds the floating objects' `modifiedTime`. Recipe 1 stays disabled, as before.
  The output for every previously accepted input is unchanged.
- Page operations: the floating object's page and the pages before it cannot
  change (deleting/reordering up to that page and inserting a page before it are
  rejected; appending at the end is allowed), `canReorder` false. This is a new
  guard: previously, page operations could shift a floating object onto another
  page.
- The compare guard did not change: if the server names a floating object,
  publishing stops.
- Projection recipe `ink-projection-v8:floating-objects`.
