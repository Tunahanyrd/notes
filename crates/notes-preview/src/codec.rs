//! `preview_codec` (Samsung SPI canvas-cache decoder) and the default
//! `previews._codec` decoder.
//!
//! Since 2026-10-27 the decoder is our own pure-Rust implementation
//! ([`crate::spi`], format described in docs/CANVAS_CACHE.md). No Samsung
//! library, emulator, interpreter or child process is involved, so the
//! provider is always available. The error codes of the historical Python
//! API are kept.
use crate::spi::{self, SpiError};
use crate::util::preview;
use notes_host::Fail;
use std::path::Path;

const MAX_PIXELS: i64 = 4 * 1024 * 1024;

/// `PreviewCodecError(code, message)`
#[derive(Debug, Clone, PartialEq)]
pub struct CodecError {
    pub code: &'static str,
    pub message: &'static str,
}

fn err(code: &'static str, message: &'static str) -> CodecError {
    CodecError { code, message }
}

/// `preview_codec.provider_available()`: the built-in decoder needs no
/// optional component. `_root` is kept for API compatibility.
pub fn provider_available(_root: &Path) -> bool {
    true
}

/// `preview_codec.decode_spi(raw, expected_size=...)` -> RGBA PNG bytes.
pub fn decode_spi(_root: &Path, raw: &[u8], expected_size: Option<(i64, i64)>) -> Result<Vec<u8>, CodecError> {
    let header = rs_ink::core::binary::inspect_spi(raw)
        .map_err(|_| err("unsupported", "Bu çizim önbelleği biçimi desteklenmiyor."))?;
    let (width, height) = (header.width as i64, header.height as i64);
    if width * height > MAX_PIXELS {
        return Err(err("too_large", "Bu sayfa önizleme boyut sınırını aşıyor."));
    }
    if header.flags != 0xe0 || header.color_index != 4 || header.reference_cache_capacity != 0 {
        return Err(err("unsupported", "Bu çizim önbelleği çeşidi henüz desteklenmiyor."));
    }
    if expected_size.is_some_and(|s| s != (width, height)) {
        return Err(err("size_mismatch", "Sayfa önbelleğinin boyut bilgisi uyuşmuyor."));
    }
    let image = spi::decode(raw).map_err(|e| match e {
        SpiError::Unsupported(_) => err("unsupported", "Bu çizim önbelleği çeşidi henüz desteklenmiyor."),
        SpiError::TooLarge => err("too_large", "Bu sayfa önizleme boyut sınırını aşıyor."),
        SpiError::Truncated | SpiError::Invalid(_) => err("decode_failed", "Sayfa önizlemesi güvenle çözümlenemedi."),
    })?;
    if (image.width as i64, image.height as i64) != (width, height) {
        return Err(err("invalid_output", "Önizleme boyutu doğrulanamadı."));
    }
    // Same PNG encoder (and bytes) as the historical worker: the codec's
    // four output bytes are written as R, G, B, A with straight alpha.
    crate::ink::measure_png(|| rs_render::pixels::rgba_png(width, height, &image.pixels))
        .map_err(|_| err("invalid_output", "Önizleme çıktısı doğrulanamadı."))
}

/// `previews._codec(raw, size)`: the default manager decoder.
pub fn decode(root: &Path, raw: &[u8], size: (i64, i64)) -> Result<Vec<u8>, Fail> {
    decode_spi(root, raw, Some(size)).map_err(|_| preview("Bu sayfanın görüntüsü çözülemedi. Metin önizlemesi korunuyor."))
}
