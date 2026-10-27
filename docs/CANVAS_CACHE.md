# Canvas-cache images (`.spi`)

Samsung Notes stores a ready-made picture of each handwritten page next to
the vector strokes (`canvasCacheDataMap` in the page XML, downloaded as a
content file). This document describes that image format as implemented by
our independent decoder in `crates/notes-preview/src/spi/`. It is written in
our own words from our analysis; no vendor code or binary is part of this
repository, and the decoder never loads one.

Status (2026-10-27):

- Implemented: the configuration Samsung Notes uses for saved page caches
  (colour index 4, header flags `0xe0`, no reference images, packet byte B
  zero, no buffer-copy shortcut), every block mode used by it.
- Validated: byte-identical decoded pixels against the original codec on
  synthetic images (encoded both by the original encoder and by our own
  constructor, see "Validation") and on one real cached page; the real page's
  PNG is byte-identical to the PNG the app had cached from the old decoder.
- Not supported (rejected, never guessed): other colour indices, other header
  flags (reduced secondary planes, reference-image/temporal coding), packet
  byte B ≠ 0, the buffer-copy shortcut, out-of-order packets.

## Container

All multi-byte fields inside packets are big-endian; the two outer block
lengths are little-endian.

```
u32le  20                     header block length
[20]   header packet
u32le  N                      data block length (= rest of the file)
[N]    data packets, one per row group, in order
```

### Header packet (20 bytes)

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | marker `0xaa` |
| 1 | 1 | packet kind `1` |
| 2 | 4 | packet length `20` |
| 6 | 1 | unused (0) |
| 7 | 2+2 | capacity width/height (0 = unspecified; otherwise ≥ image size) |
| 11 | 2 | image width |
| 13 | 2 | image height |
| 15 | 1 | colour index (`4` = three colour planes + alpha) |
| 16 | 2 | row-group size `G` (in 16-pixel block rows, 1 ≤ G ≤ ceil(H/16)) |
| 18 | 1 | number of reference images (0 for page caches) |
| 19 | 1 | flags: bit 7 A, bit 6 B, bit 5 C, bit 4 D, bits 3–0 must be zero |

Page caches use flags `0xe0` (A, B, C set; D clear). The image is divided
into 16×16 blocks: `C = ceil(W/16)` columns, `R = ceil(H/16)` rows, and
`ceil(R/G)` row groups. Sizes are limited to 32768 per side; our decoder also
caps images at 4 Mi pixels.

### Data packet

| Bits | Field |
| --- | --- |
| 0–7 | marker `0xaa` |
| 8–15 | packet kind `2` |
| 16–47 | packet length in bytes, including this prefix |
| 48–55 | byte A (written as 0, ignored) |
| 56–71 | row-group index (must equal the packet's position) |
| 72–79 | byte B (0 for page caches) |
| 80 | buffer-copy shortcut (0 for page caches) |
| 81–82 | selector `S` (used by differential blocks) |
| 83–90 | quantizer `Qc` |
| 91–98 | alternate quantizer `Qd` |
| 99–106 | signed adjustment (unused here) |
| 107–111 | must be zero |

The packet body follows at bit 112 and is read most-significant bit first.
It contains the **colour pass**: every block of the group's rows in raster
order, then (after aligning to a byte) the **alpha pass** over the same
blocks, then alignment to a byte. The packet must end exactly there.

### Variable-length codes

- *positive(k)*: `k` zero bits, a one bit, then `k` suffix bits; value
  `2^k + suffix` (1 → `1`, 2 → `010`, 5 → `00101`).
- *unary*: count of zero bits before a one bit.

### Block modes

Each block starts with a mode prefix: `1` → mode 0, `01` → mode 1,
`00xx` → mode 2 + `xx`.

| Mode | Colour pass | Alpha pass |
| --- | --- | --- |
| 0 | copy the block to the left (above when in column 0) | same |
| 1 | copy: `1` = block above; `0` + positive H + positive V = offset | same |
| 2 | differential planes | invalid |
| 3 | intra prediction with residuals | intra prediction (lossless) |
| 4 | palette + run lengths | invalid |
| 5 | literal planes (byte aligned) | literal plane (byte aligned) |

Copy offsets: H even → `H/2` blocks to the left, H odd → `H/2` blocks to the
right; V → `V-1` blocks up. The source block must lie inside the image.

## Planes, frame and output

The colour pass carries three byte planes P0, P1, P2, the alpha pass one
plane A. Decoding writes complete 16×16 blocks into a frame that has a
16-pixel border on every side; the border and all not-yet-decoded pixels
start at 128. Pixels beyond the image width/height are decoded too and
cropped at the end. The output pixel is the four bytes **P2, P0, P1, A**
(straight alpha); the preview writes them unchanged as PNG R, G, B, A.

## Literal blocks (mode 5)

Skip to the next byte boundary (the skipped bits are arbitrary), then read
256 bytes per plane in row order: P0, P1, P2 for colour, A for alpha.

## Palette blocks (mode 4, colour only)

The decoder keeps a cached palette (256 entries of three bytes), a temporary
palette, and the cached entry count and index width; count and width reset to
zero at the start of each packet, the entries persist.

Control bits: `0` reuse the cached count/width; `10` extend the cached
palette; `11` replace it. For `10`/`11` read a 4-bit index width `b`.

- `b > 0`: read `b` bits + 1 = count `N` (≤ 256). Replace reads `N` triples
  into the temporary palette; extend reads triples only for entries from the
  old count up to `N-1`, into the cached palette. Then read (`b`-bit index,
  positive run) pairs until 256 positions are filled (indices are truncated
  to 8 bits; a run may not overflow the block). Pixels take the triple of
  their index from the temporary (replace) or cached palette.
- `b = 0`: one triple is read into temporary entry 0; replace fills the block
  with it (count becomes 1), extend fills the block with *cached* entry 0 and
  sets the count to 0. Reuse with width 0 fills with cached entry 0.
- After a replace, the whole temporary palette becomes the cached palette.

## Differential blocks (mode 2, colour only)

One *reduced* bit, then three symbol streams: P0 with 256 symbols, P1 and P2
with 256 symbols, or 64 symbols (an 8×8 plane) when reduced.

**Symbols.** Keep the two most recent symbols (initially 255 and 0). Before
each symbol, if the two are equal, read positive `P` and repeat that symbol
`(P-1) mod 256` times. A symbol is `q` zeros + one + 2 bits `r`: `4q + r` for
`q ≤ 20`, `84 + r` for `q = 21, r ≤ 2`, and for `q = 21, r = 3` the next 8 bits.
Symbol `c` means a difference `d = c/2` (even) or `-(c+1)/2` (odd).

**Prediction.** Each plane is reconstructed in raster order from the sample
one row up (`distance` 16, or 8 for reduced planes with `S ≠ 0`); the first
row uses a 16-byte history per plane that persists across blocks and
packets (initially 128). A full-size block first sets the history to the
16 pixels above the block (entries 1–16 of the above edge, see "Edges"); a
reduced block sets P0's history the same way and only the *last eight* bytes
of P1/P2's history to a [1 2 1]-filtered half of the 32 above-edge pixels.
With step/period `(s, n)` = (1, 256), (2, 129), (4, 65), (1, 256) for
`S` = 0..3: `v = previous + s·d`. `S = 0` wraps modulo 256. Otherwise (reduced
planes first take the truncated remainder of `v` by `s·n`) add `s·n` if
`v < -floor(s/2)`, subtract it if `v > 255 + floor(s/2)`, and clamp to
0–255. Reduced 8×8 planes are expanded to 16×16: rows are interpolated with
weights 3:1 and 1:3 between neighbours (first and last row kept), then
columns with the rounded mean of neighbours (last column repeated).

## Intra blocks (mode 3)

### Quantizer

Colour blocks read one bit choosing `Q = Qc` (0) or `Q = min(Qc, Qd)` (1);
`Q` must be ≤ 51. Alpha always uses `Q = 0`. Colour blocks are coded in a
reversible colour space: `d = P2 - P1`, `t = P1 + floor(d/2)`, `e = P0 - t`,
`Y = t + floor(e/2)`, `E = e + 256`, `D = d + 256`; the three signed planes
Y, E, D are decoded one after another, then converted back
(`t = Y - floor(e/2)`, `P0 = clip(t + e)`, `P1 = clip(t - floor(d/2))`,
`P2 = clip(P1 + d)`, where `clip` maps a 16-bit value to 0–255 and the
16-bit minimum to 1).

### Layout

Per plane: one bit *divided*. Not divided: one 16×16 section. Divided: four
8×8 sections in the order top-left, top-right, bottom-left, bottom-right,
each with a *split* bit; a split section has four 4×4 partitions in the same
order, otherwise one 8×8 partition. For each section: the prediction mode of
every partition, then one coded-partition mask, then the coefficients of the
coded partitions.

### Prediction modes

Every plane keeps a grid of prediction-mode markers, one per 4×4 cell, for
the previous and the current block row (all start at mode 2). Markers of the
previous rows are reset to 2 at each packet; at the end of a block row the
current rows become the previous rows. Mode-3 partitions write their mode
into their cells; other block modes reset their block's cells to 2 (colour
blocks for all three colour planes). Quirk preserved: an alpha *literal*
block resets the cells of block column `4c` instead of `c`; we reject images
where that reset would fall outside the marker grid.

A partition's mode is read relative to the left neighbour cell `L` and the
above neighbour cell `A` (mode 17 counts as 2 here):

- `1`: `L` if `L = A`, otherwise one more bit selects `L` (0) or `A` (1);
- `01` + bit: mode 2 (0) or mode 17 (1);
- `00` + 4 bits: index into the ascending list 0..17 without `L` and 2.

Mode 17 is planar, mode 2 is DC (with boundary smoothing), modes 0 and 1 are
vertical and horizontal, the rest are angular directions with slopes
±5, ±13, ±21 and 32 (in 1/32 pixel), an HEVC-like scheme with 17 directions.

### Edges

Each block gets a 33-sample above edge (corner + 32) and a 33-sample left edge
per plane, from the frame. Inside a row group's first block row nothing above
is used. Missing neighbours are synthesised from the available samples
(repeat the last sample, take the neighbour's first sample, or average the two
corners) depending on which of left/above/above-right/above-left blocks exist.
Colour edges are converted to Y/E/D before prediction. Within a block, a
partition's edges come from the block edges and from already reconstructed
partitions; an extension that reaches an undecoded partition repeats the last
available sample. Quirk preserved: the 4×4 partition at block offset (4, 4)
reuses the above-edge extension left by the partition before it.

Before angular prediction some modes smooth both edges with a [1 2 1] filter
(4×4: modes 3, 6, 9; 8×8: modes 3–9; 16×16: modes 3–16), sharing a smoothed
corner.

### Coefficients

The mask: for 4×4 partitions a positive code indexes a 64-entry table
(chosen by `Q` class: 0–21, 22–26, 27–31, 32–36, 37–50, 51 → first table);
for larger partitions a unary code indexes an 8-entry table. With `P`
partitions, partition `p` is coded when mask bit `P + 1 - p` is set.

A coded partition is a positive token count `N` (≤ area) and `N` tokens:
positive code `M` + sign bit. `M < 128` indexes a fixed table of
(level, zero-run) pairs. Escapes: with `Q = 0`, `M - 128` gives
`run = (M-128) mod area`, `level = (M-128) div area + 1`; with `Q > 0`,
`level = ((M-1) >> 8) - 1`, `run = (M-1) & 255`. Positions follow a scan:
with `Q > 0` always zigzag; with `Q = 0` zigzag for 16×16 and mode 17, else
rows / columns / zigzag by prediction mode. A partition whose only token sits
at position 0 is "DC only".

### Reconstruction

- `Q = 0`: for prediction mode 0 (1) the residuals are summed down columns
  (along rows) first; the sample is prediction + residual in 16-bit wrapping
  arithmetic (alpha then converts to a byte with `clip`).
- `Q > 0` (colour only): coefficients are dequantised with H.264/HEVC-style
  scale tables (`Q mod 6`, shift by `Q div 6`), inverse transformed (4×4 and
  8×8 integer butterflies with a transposed output, 16×16 HEVC basis), and
  added to the prediction; results clamp to 0–255 for Y and 0–511 for E/D. A
  DC-only partition adds the rounded DC value to every sample.

## Validation

- 894 images encoded by the original encoder from synthetic patterns
  (solid, gradients, checker, random, stroke-like drawings on transparent and
  white backgrounds; sizes 1×1 to 240×160; 14 quality settings): our pixels are
  byte-identical to the original decoder's for all of them.
- 97 images built by our own stream constructor (`tests/spi_support`), which
  exercise what the encoder did not emit (all 18 prediction modes, reduced
  differential planes, alpha literals, multiple packets): byte-identical to
  the original decoder; these are the committed fixtures and their recorded
  pixel digests.
- One real page cache: byte-identical pixels; the resulting PNG equals, byte
  for byte, both PNGs the app had cached for it earlier (light and
  dark-converted).
- Robustness: every truncation and ~300 000 random corruptions of the fixtures
  decode without a panic (debug build with overflow checks).

The original decoder was only used as a local comparison oracle during
development; it is not, and never was, part of this repository.
