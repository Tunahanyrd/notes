//! In-process SVG rasterizer (resvg/tiny-skia, pure Rust) for renderer SVG.
use resvg::{tiny_skia, usvg};

/// Straight-alpha RGBA of `svg` at `width`×`height` (scaled from the SVG size).
pub fn rasterize(svg: &[u8], width: u32, height: u32) -> Result<Vec<u8>, &'static str> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg, &options).map_err(|_| "svg")?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("size")?;
    let size = tree.size();
    let transform = tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(out)
}
