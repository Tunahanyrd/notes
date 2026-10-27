//! Cached page images: the narrow RGBA8 PNG validator, the decoder used to
//! compose PDF canvases and convert dark caches, and the PPM reader.
//! Input: two size bytes, then the image bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let (w, h) = (1 + data[0] as i64, 1 + data[1] as i64);
    let img = &data[2..];
    let _ = rs_render::pixels::png_valid(img, w, h);
    if let Ok(px) = rs_render::pixels::rgba(img, w, h) {
        let _ = rs_render::pixels::rgba_png(w, h, &px);
    }
    let _ = rs_render::pixels::unfilter_png(img, w, h, 1 << 22);
    let _ = rs_render::pixels::parse_ppm(img, w as usize, h as usize);
    let _ = notes_preview::png_valid(img, w, h);
});
