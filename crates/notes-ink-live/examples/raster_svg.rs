//! Fidelity helper (tools/ink_live_fidelity.mjs): rasterize a page SVG from
//! tools/ink-renderer exactly like the live core rasterizes its own SVG
//! (page background removed, resvg at `scale` device pixels per page unit).
//! Usage: raster_svg <in.svg> <page_width> <page_height> <scale> <out.bin>
//! Output: x, y, width, height (i32 LE) followed by straight-alpha RGBA.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() == 6, "usage: raster_svg <in.svg> <page_width> <page_height> <scale> <out.bin>");
    let svg = std::fs::read_to_string(&args[1]).expect("svg");
    let page = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let scale: f64 = args[4].parse().unwrap();
    let raster = notes_ink_live::raster_svg(&notes_ink_live::strip_background(svg), page, scale).expect("raster");
    let mut out = Vec::with_capacity(16 + raster.rgba.len());
    for v in [raster.x, raster.y, raster.width as i32, raster.height as i32] {
        out.extend(v.to_le_bytes());
    }
    out.extend(&raster.rgba);
    std::fs::write(&args[5], out).expect("write");
}
