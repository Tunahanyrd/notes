# ink-render

Source-backed optional Linux renderer adapter, GPL-3.0. Uses **twangodev/sdocx**
commit `41e9c1099b29a48dc7320501573d7c4858f09303` from
`references/upstream/sdocx`, licensed GPL-3.0. The build checks that revision and
that its crate sources are unmodified. See LICENSE and the upstream source at
https://github.com/twangodev/sdocx/tree/41e9c1099b29a48dc7320501573d7c4858f09303.
No Samsung binary is included or executed by this renderer. Preserve applicable
source and license notices when distributing this component or a combined build.

Build: `CARGO_BUILD_JOBS=1 cargo build --manifest-path tools/ink-renderer/Cargo.toml`.
Separate Cargo workspace; does not modify the main workspace or upstream.

One bounded JSON object on stdin, SVG on stdout, static errors on stderr.
The private Python adapter constructs the full typed sdocx Stroke fields from
validated source metadata. Input/output are private note contents: never log.
Only admitted saved Fountain V14 (`14;`) and V16 (`18;0;100;`) profiles render.
Any approximate profile, unsupported inventory or budget excess rejects the
whole request. InkPen2 is intentionally rejected: its native reconstructed
profile needs separate screen/stylus/state inputs absent from our capture path.

Native-derived geometry does not mean identical GPU pixels. SVG edge sampling
and the bounded local rasterizer can differ. This is not a general document
renderer: text, PDF, embedded artwork and unsupported pen mixtures are rejected
by the Python adapter instead of being silently omitted.
