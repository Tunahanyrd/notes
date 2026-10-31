//! In-process SVG rasterizer (resvg/tiny-skia, pure Rust) for renderer SVG.
use resvg::{tiny_skia, usvg};

/// Physical size of an ink tile. Edges outside the page are transparent.
pub const INK_TILE_SIZE: u32 = 512;

/// Allowed device-pixels-per-page-unit buckets, stored in half units so cache
/// identities and query validation never depend on floating point text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InkTileScale(u8);

impl InkTileScale {
    pub fn from_half_units(value: u8) -> Option<Self> {
        matches!(value, 2 | 3 | 4 | 6 | 8).then_some(Self(value))
    }

    /// Smallest bucket that is no smaller than the requested scale. Refuse
    /// oversized or non-finite requests instead of silently undersampling.
    pub fn for_scale(scale: f64) -> Option<Self> {
        if !scale.is_finite() || scale <= 0.0 { return None; }
        [2, 3, 4, 6, 8].into_iter().find(|v| f64::from(*v) / 2.0 >= scale).map(Self)
    }

    pub fn half_units(self) -> u8 { self.0 }
    pub fn value(self) -> f32 { f32::from(self.0) / 2.0 }
}

/// A validated page tile. The source page has the same 4 Mi-pixel limit as
/// the existing preview; each result is 512 x 512 pixels. The renderer also
/// uses a bounded 1536 x 1536 scratch surface as an antialiasing guard band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InkTile {
    scale: InkTileScale,
    column: u32,
    row: u32,
    page_width: u32,
    page_height: u32,
}

impl InkTile {
    pub fn scale(self) -> InkTileScale { self.scale }
    pub fn column(self) -> u32 { self.column }
    pub fn row(self) -> u32 { self.row }
    pub fn new(page_width: u32, page_height: u32, scale: InkTileScale, column: u32, row: u32) -> Option<Self> {
        if page_width == 0 || page_height == 0 || page_width > 32768 || page_height > 32768
            || u64::from(page_width) * u64::from(page_height) > crate::pixels::MAX_PIXELS as u64 {
            return None;
        }
        let scaled_width = (u64::from(page_width) * u64::from(scale.0)).div_ceil(2);
        let scaled_height = (u64::from(page_height) * u64::from(scale.0)).div_ceil(2);
        if u64::from(column) * u64::from(INK_TILE_SIZE) >= scaled_width
            || u64::from(row) * u64::from(INK_TILE_SIZE) >= scaled_height { return None; }
        Some(Self { scale, column, row, page_width, page_height })
    }

    pub fn grid(page_width: u32, page_height: u32, scale: InkTileScale) -> Option<(u32, u32)> {
        Self::new(page_width, page_height, scale, 0, 0)?;
        let width = (u64::from(page_width) * u64::from(scale.0)).div_ceil(2);
        let height = (u64::from(page_height) * u64::from(scale.0)).div_ceil(2);
        Some((width.div_ceil(u64::from(INK_TILE_SIZE)) as u32, height.div_ceil(u64::from(INK_TILE_SIZE)) as u32))
    }
}

/// Rasterize a transparent, stroke-only SVG with the live renderer's
/// scale/translate and straight-alpha conversion. The caller must supply an
/// SVG from the trusted ink renderer with its background removed.
pub fn rasterize_ink_tile(svg: &[u8], tile: InkTile) -> Result<Vec<u8>, &'static str> {
    if svg.len() > 16 * 1024 * 1024 { return Err("svg size"); }
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).map_err(|_| "svg")?;
    let size = tree.size();
    if size.width() != tile.page_width as f32 || size.height() != tile.page_height as f32 {
        return Err("page size");
    }
    const PAD: u32 = INK_TILE_SIZE;
    const RENDER_SIZE: u32 = INK_TILE_SIZE + 2 * PAD;
    let mut pixmap = tiny_skia::Pixmap::new(RENDER_SIZE, RENDER_SIZE).ok_or("size")?;
    let x = tile.column * INK_TILE_SIZE;
    let y = tile.row * INK_TILE_SIZE;
    let scale = tile.scale.value();
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, PAD as f32 - x as f32, PAD as f32 - y as f32);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let pixels = pixmap.take();
    let mut out = vec![0; (INK_TILE_SIZE * INK_TILE_SIZE * 4) as usize];
    for row in 0..INK_TILE_SIZE as usize {
        let from = ((row + PAD as usize) * RENDER_SIZE as usize + PAD as usize) * 4;
        let to = row * INK_TILE_SIZE as usize * 4;
        out[to..to + INK_TILE_SIZE as usize * 4].copy_from_slice(&pixels[from..from + INK_TILE_SIZE as usize * 4]);
    }
    let right = (u64::from(tile.page_width) * u64::from(tile.scale.0)).div_ceil(2) as u32;
    let bottom = (u64::from(tile.page_height) * u64::from(tile.scale.0)).div_ceil(2) as u32;
    for (i, px) in out.chunks_exact_mut(4).enumerate() {
        let px_x = x + (i as u32 % INK_TILE_SIZE);
        let px_y = y + (i as u32 / INK_TILE_SIZE);
        if px_x >= right || px_y >= bottom { px.fill(0); continue; }
        let a = u32::from(px[3]);
        if a != 0 && a != 255 {
            for channel in &mut px[..3] { *channel = ((u32::from(*channel) * 255 + a / 2) / a).min(255) as u8; }
        }
    }
    Ok(out)
}

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

#[cfg(test)]
mod ink_tile_tests {
    use super::*;

    #[test]
    fn bucket_selection_and_untrusted_bounds() {
        assert_eq!(InkTileScale::for_scale(1.0).unwrap().half_units(), 2);
        assert_eq!(InkTileScale::for_scale(1.01).unwrap().half_units(), 3);
        assert_eq!(InkTileScale::for_scale(1.5).unwrap().half_units(), 3);
        assert_eq!(InkTileScale::for_scale(1.501).unwrap().half_units(), 4);
        for n in [0.0, -1.0, f64::NAN, f64::INFINITY, 4.01] { assert!(InkTileScale::for_scale(n).is_none()); }
        assert!(InkTileScale::from_half_units(5).is_none());
        let scale = InkTileScale::from_half_units(3).unwrap();
        assert_eq!(InkTile::grid(600, 520, scale), Some((2, 2)));
        assert!(InkTile::new(600, 520, scale, 2, 0).is_none());
        assert!(InkTile::new(600, 520, scale, 0, u32::MAX).is_none());
        assert!(InkTile::new(0, 1, scale, 0, 0).is_none());
        assert!(InkTile::new(32768, 32768, scale, 0, 0).is_none());
    }

    #[test]
    fn transparent_layer_removes_only_the_validated_page_background() {
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="520" viewBox="0 0 600 520"><rect x="0" y="0" width="600" height="520" fill="#ffffff"/><path d="M100 100 L200 100" stroke="#333" stroke-width="6"/></svg>"##;
        let svg = crate::compose::transparent_ink_svg(source, 600, 520).unwrap();
        let scale = InkTileScale::from_half_units(2).unwrap();
        let pixels = rasterize_ink_tile(svg.as_bytes(), InkTile::new(600, 520, scale, 0, 0).unwrap()).unwrap();
        assert_eq!(pixels[3], 0, "background must stay transparent");
        assert!(pixels[((100 * 512 + 150) * 4 + 3) as usize] > 0, "the stroke must remain visible");
        assert!(crate::compose::transparent_ink_svg(source.replace("0 0 600 520", "0 1 600 520").as_str(), 600, 520).is_err());
        assert!(crate::compose::transparent_ink_svg(source.replace("width=\"600\" height=\"520\" fill", "width=\"10\" height=\"520\" fill").as_str(), 600, 520).is_err());
    }

    #[test]
    fn tiles_match_full_raster_across_seams_and_clip_edges() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="520"><path d="M10 150 C300 0 400 500 590 400" fill="none" stroke="#3269a2" stroke-width="7" stroke-linecap="round"/><rect x="580" y="500" width="100" height="100" fill="#f00"/></svg>"##;
        let scale = InkTileScale::from_half_units(3).unwrap();
        // Independent full-page render with the live renderer's straight-alpha
        // rounding. `rasterize` uses tiny-skia's different demultiplication,
        // which can differ substantially in RGB at near-zero alpha.
        let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).unwrap();
        let mut full_map = tiny_skia::Pixmap::new(900, 780).unwrap();
        resvg::render(&tree, tiny_skia::Transform::from_scale(1.5, 1.5), &mut full_map.as_mut());
        let mut full = full_map.take();
        for pixel in full.chunks_exact_mut(4) {
            let a = u32::from(pixel[3]);
            if a != 0 && a != 255 { for channel in &mut pixel[..3] { *channel = ((u32::from(*channel) * 255 + a / 2) / a).min(255) as u8; } }
        }
        for row in 0..2 { for col in 0..2 {
            let tile = rasterize_ink_tile(svg, InkTile::new(600, 520, scale, col, row).unwrap()).unwrap();
            assert_eq!(tile.len(), (INK_TILE_SIZE * INK_TILE_SIZE * 4) as usize);
            for y in 0..INK_TILE_SIZE { for x in 0..INK_TILE_SIZE {
                let i = ((y * INK_TILE_SIZE + x) * 4) as usize;
                let (gx, gy) = (col * INK_TILE_SIZE + x, row * INK_TILE_SIZE + y);
                if gx >= 900 || gy >= 780 { assert_eq!(&tile[i..i+4], &[0, 0, 0, 0]); continue; }
                let j = ((gy * 900 + gx) * 4) as usize;
                assert_eq!(&tile[i..i+4], &full[j..j+4], "tile {col},{row} pixel {gx},{gy}");
            }}
        }}
        assert!(rasterize_ink_tile(svg, InkTile::new(500, 500, scale, 0, 0).unwrap()).is_err());
    }

    #[test]
    fn long_curve_does_not_change_when_rendered_as_a_tile() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="2200" height="1400"><path d="M 4 700 C 1000 -500 1400 1900 2190 700" fill="none" stroke="#3269a2" stroke-width="5"/></svg>"##;
        let scale = InkTileScale::from_half_units(3).unwrap();
        let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).unwrap();
        let mut full = tiny_skia::Pixmap::new(3300, 2100).unwrap();
        resvg::render(&tree, tiny_skia::Transform::from_scale(1.5, 1.5), &mut full.as_mut());
        let mut full = full.take();
        for pixel in full.chunks_exact_mut(4) {
            let a = u32::from(pixel[3]);
            if a != 0 && a != 255 { for channel in &mut pixel[..3] { *channel = ((u32::from(*channel) * 255 + a / 2) / a).min(255) as u8; } }
        }
        let tile = rasterize_ink_tile(svg, InkTile::new(2200, 1400, scale, 3, 2).unwrap()).unwrap();
        for y in 0..512usize { for x in 0..512usize {
            let i = (y * 512 + x) * 4;
            let j = (((y + 1024) * 3300) + x + 1536) * 4;
            assert_eq!(&tile[i..i+4], &full[j..j+4], "pixel {},{}", x + 1536, y + 1024);
        }}
    }
}
