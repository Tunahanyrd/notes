# notes-ink-live

Live pen preview, GPL-3.0. The stroke under the pen is drawn with the same
renderer that saved pages use: the pinned **twangodev/sdocx** commit
`41e9c1099b29a48dc7320501573d7c4858f09303` (`references/upstream/sdocx`,
`tools/fetch_sdocx.sh`) and the same resvg version (`=0.49.0`, same as
crates/rs-render). The algorithm was not rewritten; this crate only reproduces
the writer's input:

- `rs-ink` capture quantization (Q5 position, Q12 pressure, float32 accumulation);
  `src/quantize_expected.txt` was taken from the Python oracle (`normalize_capture`).
- Template pen: the `rendering` object the page renderer sees
  (`GET /api/notes/:id/ink-pens`); stylus channels are dropped (the writer
  drops them too); when color/width changes, recipe 3 rules (fixedWidth, tolerance).
- First stroke on an empty page: recipe 4 FountainPen `18;0;100;`, tool 3.
- tools/ink-renderer `calibrated` settings (InkPen2 `9;`, profile 1440×2304).

It is a separate Cargo workspace (not a member of the root workspace; the GPL
source and lockfile do not enter the engine build).

## Build

```sh
tools/build_ink_wasm.sh            # desktop/public/ink-live.wasm (Vite → dist/)
cd desktop && npm run build
```

Requires the `wasm32-unknown-unknown` std. Distribution Rust packages often lack it:
`rustup target add wasm32-unknown-unknown`, or an isolated rustup that includes
this target via `RUSTUP_HOME`/`CARGO_HOME`. The build uses `--locked` and
`--remap-path-prefix`; two clean builds with the same source, the same toolchain
and the same registry source were measured to produce identical `.wasm` bytes.
The output is in `.gitignore`; no prebuilt `.wasm` is committed to the
repository. If the file is missing, the UI silently falls back to the old SVG stroke.

The UI runs the module in a Web Worker (`desktop/src/inkLiveWorker.ts`).
The CSP needs `script-src 'wasm-unsafe-eval'` (added to the engine's and
Tauri's CSP; string `eval` is not enabled).

## C-ABI

`src/wasm.rs`: `ink_alloc/ink_free`, `ink_pen_new(json)`, `ink_render(pen,
samples f64×4, n, scale)`, `ink_out_rect/ptr/len` (straight-alpha RGBA), `ink_error`.
No wasm-bindgen.

## Verification

- `cargo test --release` (12 tests): quantization oracle equality, pressure → width
  (stylus Fountain), first-stroke mouse profile, recipe 3, rejection of
  unsupported pens, determinism, color.
- `node tools/ink_live_fidelity.mjs`: for the same synthetic strokes, the `.wasm`
  output and the local path (Python oracle quantization → tools/ink-renderer →
  resvg) are compared pixel by pixel at the same scale.
- Playwright `desktop/tests/ink-live.spec.ts`.

Limits: the drawing is the local renderer's output, not Samsung GPU pixels.
Because a new InkPen2 stroke carries no stylus channel, sdocx's approximate
InkPen2 path draws it; after saving, the page previewer draws the same stroke
via the same path (notes-preview `admitted`; stylus-less InkPen2 has been
accepted since 2026-10-27; previously the whole page was rejected).
