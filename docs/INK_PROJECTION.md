# Ink read contract

`project_ink(snapshot)` returns bounded ink geometry without network access or file writes:

```text
{
  schemaVersion: 1,
  pages: [{id, index, width, height, strokes: [
    {id, kind: "fountain" | "pen", colorARGB, size,
     points: [{x, y, pressure, timeMs}]}
  ]}],
  capabilities: {appendInk: boolean},
  reason: null | string,
  rendering: {nativeEquivalent: false, mode: "geometry-only"}
}
```

X/Y are in document units; `size` is the original SDK pen size and is not
equivalent to a CSS stroke width. Point order and layer order are preserved;
page strokes are flattened in the layers' document order. `timeMs` carries the
recorded native relative time unchanged; the first recorded sample does not have
to be zero in every note. A separate HTTP endpoint or UI integration is outside
the scope of this module.

The single source resource/commit, canonical UUID, binary filename/time, native
UUID/time/hash, string table, layer/page ordered hash and noteTree match are
checked. If the source `DocumentSnapshot.resource_xml` and the raw MIME XML
diverge, the projection is rejected. Signed URLs, media paths, native binary or
raw advanced-setting data are not included in the DTO. The DTO itself is private
note data; it must not be logged or written to shared test records.

The initial limit is 32 pages, 256 strokes and 200,000 points in total. Only
basic FountainPen/InkPen2, orientation=0 and the supported normal stroke frame
profile are accepted. An unknown pen or a mixed unsupported object inventory is
not silently dropped to show an incomplete page: `pages=[]`, `appendInk=false`.
A native empty layer whose tree objectList is never written is supported.

For notes that contain a cache but whose point inventory is readable, geometry
may be returned; `appendInk=false` remains. The `appendInk` gate is not tied to
a separate approximate list of conditions: the existing `prepare_ink_append`
producer is tried **offline** with a synthetic center point, and the candidate
is discarded and never published anywhere. This check reuses the source
profile, metadata and hash guards. The HTTP integration must cache the
projection result for the same note revision/commit; it must not re-decode MIME
on every UI poll.

## Limits of visual fidelity

This DTO is not an SVG/canvas directly equivalent to the native pen appearance.
The existing Samsung cache/PDF preview path must continue to be used to show
the original page. Drawing the center line with `lineWidth=size` cannot be
presented to the user as the real pen rendering. The presence of a pressure
channel alone does not mean a correct pressure-brush renderer has been built.

The `stroke-rendering-findings.md` source of the pinned sdocx research narrows
down the V16 geometry path for the FountainPen `18;0;100;` setting:
sample-acceptance/tolerance, a three-sample direction/geometry history,
pressure, tilt, tool type, time-based width smoothing, midpoint quadratic curves
and distance-spaced circular stamps. Redrawing a saved stroke runs the width
accumulation and smoothing stages through the native entry `0x7851c`. The
pinned prior art contains an independent Rust geometry port and native oracle
records; this does not mean our new Python projection renders them or covers
every brush version.

It is also documented that the prior art's general fallback, pressure-scaled
straight SVG segments, can deviate from the real native geometry at corners and
width transitions. A general midpoint spline without sample filtering/width
law/tolerance is not an exact counterpart either. Edge antialiasing and
compositor equality require verification separate from geometry equality.

Our own new evidence is limited to the channel encoder/decoder and the
basic-pen bbox: the `ink-freehand-v1` native reducer8/bbox48 match. For the
projection, 6 synthetic tests and the read of 2 pages/2 strokes/append-profile
from the controlled 007 private snapshot succeeded. Personal geometry, IDs and
titles were not included in the research records. There is no claim of renderer
or live ink write success.
