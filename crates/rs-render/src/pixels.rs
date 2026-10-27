//! Pure-Rust page pixel core (no PyO3): the narrow RGBA8/filter-0 PNG contract,
//! filtered PNG canonicalisation, box downsampling, dark-to-light conversion,
//! PDF canvas compositing and PPM placement. Byte-identical to the Python
//! implementations (PNG IDAT uses the system zlib at level 6, like CPython).
use flate2::{Compress, Compression, Crc, Decompress, FlushCompress, FlushDecompress, Status};

pub const MAX_INPUT: usize = 8 * 1024 * 1024;
pub const MAX_PIXELS: i64 = 4 * 1024 * 1024;
pub const MAX_OUTPUT: usize = 64 * 1024 * 1024;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = Crc::new();
    for part in parts {
        crc.update(part);
    }
    crc.sum()
}

fn be32(raw: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// zlib (level 6, 32K window, memLevel 8) of the whole buffer; same stream as
/// CPython's `zlib.compressobj(6)` fed in any chunking.
pub fn deflate6(raw: &[u8]) -> Vec<u8> {
    let mut encoder = Compress::new(Compression::new(6), true);
    let mut out = Vec::with_capacity(raw.len() / 8 + 1024);
    loop {
        let consumed = encoder.total_in() as usize;
        if out.capacity() - out.len() < 65536 {
            out.reserve(out.capacity().max(65536));
        }
        let status = encoder
            .compress_vec(&raw[consumed..], &mut out, FlushCompress::Finish)
            .expect("deflate");
        if status == Status::StreamEnd {
            return out;
        }
    }
}

/// preview_worker.rgba_png. Err carries the Python ValueError message.
pub fn rgba_png(width: i64, height: i64, pixels: &[u8]) -> Result<Vec<u8>, &'static str> {
    let area = width as i128 * height as i128;
    if !(0 < area && area <= MAX_PIXELS as i128) || pixels.len() as i128 != area * 4 {
        return Err("pixel bounds");
    }
    let stride = width as usize * 4;
    // Stream filter byte + row into zlib (no full-size filtered copy).
    let mut encoder = Compress::new(Compression::new(6), true);
    let mut packed: Vec<u8> = Vec::with_capacity(pixels.len() / 16 + 4096);
    let feed = |encoder: &mut Compress, packed: &mut Vec<u8>, mut input: &[u8], flush: FlushCompress| {
        loop {
            if packed.capacity() - packed.len() < 65536 {
                packed.reserve(packed.capacity().max(65536));
            }
            let before = encoder.total_in();
            let status = encoder.compress_vec(input, packed, flush).expect("deflate");
            input = &input[(encoder.total_in() - before) as usize..];
            let done = match flush {
                FlushCompress::Finish => status == Status::StreamEnd,
                _ => input.is_empty() && packed.capacity() > packed.len(),
            };
            if done {
                return;
            }
        }
    };
    for row in pixels.chunks_exact(stride) {
        feed(&mut encoder, &mut packed, &[0], FlushCompress::None);
        feed(&mut encoder, &mut packed, row, FlushCompress::None);
    }
    feed(&mut encoder, &mut packed, &[], FlushCompress::Finish);
    if packed.len() > MAX_OUTPUT - 100 {
        return Err("PNG bounds");
    }
    let mut out = Vec::with_capacity(packed.len() + 64);
    out.extend_from_slice(SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &packed);
    chunk(&mut out, b"IEND", b"");
    if out.len() > MAX_OUTPUT {
        return Err("PNG bounds");
    }
    Ok(out)
}

/// previews._png_valid: IHDR(RGBA8), consecutive IDAT, IEND; all CRCs.
pub fn png_valid(raw: &[u8], width: i64, height: i64) -> bool {
    let area = width as i128 * height as i128;
    if !(57 <= raw.len() && raw.len() <= MAX_OUTPUT) || &raw[..8] != SIGNATURE
        || !(0 < area && area <= MAX_PIXELS as i128) || width.min(height) <= 0
    {
        return false;
    }
    let (mut position, mut chunks, mut image_bytes) = (8usize, 0usize, 0usize);
    while position + 12 <= raw.len() && chunks < 1024 {
        let length = be32(raw, position) as usize;
        let kind = &raw[position + 4..position + 8];
        let end = position + 12 + length;
        if end > raw.len() {
            return false;
        }
        let payload = &raw[position + 8..position + 8 + length];
        if crc32(&[kind, payload]) != be32(raw, position + 8 + length) {
            return false;
        }
        if chunks == 0 {
            if kind != b"IHDR" || length != 13 || be32(raw, position + 8) as i64 != width
                || be32(raw, position + 12) as i64 != height || &payload[8..] != b"\x08\x06\x00\x00\x00"
            {
                return false;
            }
        } else if kind == b"IDAT" {
            image_bytes += length;
        } else if kind == b"IEND" {
            return length == 0 && image_bytes > 0 && end == raw.len();
        } else {
            return false;
        }
        position = end;
        chunks += 1;
    }
    false
}

pub enum Inflate {
    Ok(Vec<u8>),
    /// zlib.error while decompressing.
    Corrupt,
    /// Wrong size, truncated stream or trailing data.
    Size,
}

/// `decompressobj().decompress(data, expected + 1)` plus the eof /
/// unused_data / unconsumed_tail checks of the Python callers.
pub fn inflate_exact(data: &[u8], expected: usize) -> Inflate {
    let mut decoder = Decompress::new(true);
    let limit = expected + 1;
    let mut out = Vec::with_capacity(limit);
    let mut ended = false;
    loop {
        let before_in = decoder.total_in();
        let before_out = out.len();
        let consumed = before_in as usize;
        match decoder.decompress_vec(&data[consumed..], &mut out, FlushDecompress::None) {
            Err(_) => return Inflate::Corrupt,
            Ok(Status::StreamEnd) => {
                ended = true;
                break;
            }
            Ok(_) => {}
        }
        if out.len() >= limit {
            break;
        }
        if decoder.total_in() == before_in && out.len() == before_out {
            break; // needs more input than there is: truncated stream
        }
    }
    if out.len() != expected || !ended || decoder.total_in() as usize != data.len() {
        return Inflate::Size;
    }
    Inflate::Ok(out)
}

pub enum RgbaError {
    Invalid,
    Corrupt,
    Size,
    Filter,
}

impl RgbaError {
    /// pdf_background._rgba messages (PdfBackgroundError).
    pub fn message(&self) -> &'static str {
        match self {
            RgbaError::Invalid => "Sayfa katmanının boyutu doğrulanamadı.",
            RgbaError::Corrupt => "Sayfa katmanı çözülemedi.",
            RgbaError::Size => "Sayfa katmanı boyut sınırını aşıyor.",
            RgbaError::Filter => "Sayfa katmanının filtresi desteklenmiyor.",
        }
    }
}

/// pdf_background._rgba: straight RGBA bytes of a contract PNG.
pub fn rgba(png: &[u8], width: i64, height: i64) -> Result<Vec<u8>, RgbaError> {
    if !png_valid(png, width, height) {
        return Err(RgbaError::Invalid);
    }
    let mut compressed = Vec::new();
    let mut position = 8;
    while position < png.len() {
        let size = be32(png, position) as usize;
        if &png[position + 4..position + 8] == b"IDAT" {
            compressed.extend_from_slice(&png[position + 8..position + 8 + size]);
        }
        position += size + 12;
    }
    let (w, h) = (width as usize, height as usize);
    let stride = w * 4 + 1;
    let raw = match inflate_exact(&compressed, stride * h) {
        Inflate::Ok(raw) => raw,
        Inflate::Corrupt => return Err(RgbaError::Corrupt),
        Inflate::Size => return Err(RgbaError::Size),
    };
    let mut raw = raw;
    // Drop the filter bytes in place (rows shift left by one byte each).
    for y in 0..h {
        if raw[y * stride] != 0 {
            return Err(RgbaError::Filter);
        }
        raw.copy_within(y * stride + 1..(y + 1) * stride, y * (stride - 1));
    }
    raw.truncate(w * h * 4);
    Ok(raw)
}

/// page_image.light_from_dark on raw RGBA (in place).
pub fn light_from_dark_pixels(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        let shift = 255 - r.max(g).max(b) - r.min(g).min(b);
        px[0] = (r + shift) as u8;
        px[1] = (g + shift) as u8;
        px[2] = (b + shift) as u8;
    }
}

pub enum DownsampleError {
    Length,
    ZeroDivision,
    ByteRange,
}

/// page_image.box_downsample: alpha-weighted box filter on `background`.
pub fn box_downsample(source: &[u8], width: usize, height: usize, target_w: usize, target_h: usize,
                      background: i64) -> Result<Vec<u8>, DownsampleError> {
    if source.len() != width * height * 4 || target_w == 0 || target_h == 0 {
        return Err(DownsampleError::Length);
    }
    if target_w > width || target_h > height {
        // Some boxes would be empty: Python divides by zero.
        return Err(DownsampleError::ZeroDivision);
    }
    let cols: Vec<(usize, usize)> = (0..target_w).map(|x| (x * width / target_w, (x + 1) * width / target_w)).collect();
    let mut out = vec![0u8; target_w * target_h * 4];
    let mut sums = vec![[0u64; 4]; target_w];
    for y in 0..target_h {
        let (top, bottom) = (y * height / target_h, (y + 1) * height / target_h);
        for s in sums.iter_mut() {
            *s = [0; 4];
        }
        for row in top..bottom {
            let line = &source[row * width * 4..(row + 1) * width * 4];
            for (x, &(left, right)) in cols.iter().enumerate() {
                let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
                for px in line[left * 4..right * 4].chunks_exact(4) {
                    let alpha = px[3] as u64;
                    r += px[0] as u64 * alpha;
                    g += px[1] as u64 * alpha;
                    b += px[2] as u64 * alpha;
                    a += alpha;
                }
                let s = &mut sums[x];
                s[0] += r;
                s[1] += g;
                s[2] += b;
                s[3] += a;
            }
        }
        for (x, &(left, right)) in cols.iter().enumerate() {
            let count = ((bottom - top) * (right - left)) as i64;
            let divisor = count * 255;
            let s = sums[x];
            let fill = background * (divisor - s[3] as i64);
            let target = (y * target_w + x) * 4;
            for c in 0..3 {
                let value = (s[c] as i64 + fill + divisor / 2).div_euclid(divisor);
                if !(0..=255).contains(&value) {
                    return Err(DownsampleError::ByteRange);
                }
                out[target + c] = value as u8;
            }
            out[target + 3] = 255;
        }
    }
    Ok(out)
}

/// pdf_background.compose_pdf_canvas on raw RGBA; Err when the layers are
/// not an opaque page plus a proven-transparent straight-alpha canvas.
pub fn compose(under: &mut [u8], over: &[u8]) -> Result<(), ()> {
    if under.chunks_exact(4).any(|p| p[3] != 255) || !over.chunks_exact(4).any(|p| p[3] == 0) {
        return Err(());
    }
    for (u, o) in under.chunks_exact_mut(4).zip(over.chunks_exact(4)) {
        let alpha = o[3] as u32;
        if alpha == 0 {
            continue;
        }
        if alpha == 255 {
            u[..3].copy_from_slice(&o[..3]);
        } else {
            for c in 0..3 {
                u[c] = ((o[c] as u32 * alpha + u[c] as u32 * (255 - alpha) + 127) / 255) as u8;
            }
        }
    }
    Ok(())
}

/// Place a PPM's RGB rows into an opaque white canvas at (left, top).
pub fn place_rgb(rgb: &[u8], width: usize, height: usize, canvas_w: usize, canvas_h: usize,
                 left: usize, top: usize) -> Vec<u8> {
    let mut pixels = vec![0xffu8; canvas_w * canvas_h * 4];
    for row in 0..height {
        let line = &rgb[row * width * 3..(row + 1) * width * 3];
        let start = ((row + top) * canvas_w + left) * 4;
        for (x, px) in line.chunks_exact(3).enumerate() {
            pixels[start + x * 4..start + x * 4 + 3].copy_from_slice(px);
        }
    }
    pixels
}

/// pdf_background._read_ppm on already-read file bytes (size checked by caller).
/// Err(true) = dimension message, Err(false) = generic message.
pub fn parse_ppm(data: &[u8], width: usize, height: usize) -> Result<&[u8], bool> {
    let mut header: Vec<&[u8]> = Vec::new();
    let mut cursor = 0;
    for _ in 0..16 {
        // readline(128): up to and including '\n', at most 128 bytes.
        let rest = &data[cursor..];
        let limit = rest.len().min(128);
        let length = rest[..limit].iter().position(|&b| b == b'\n').map(|i| i + 1).unwrap_or(limit);
        let line = &rest[..length];
        if line.is_empty() || line.len() >= 128 {
            return Err(false);
        }
        cursor += length;
        if line.starts_with(b"#") {
            continue;
        }
        header.extend(line.split(|b| b.is_ascii_whitespace() || *b == 0x0b || *b == 0x0c).filter(|t| !t.is_empty()));
        if header.len() >= 4 {
            break;
        }
    }
    let w = width.to_string();
    let h = height.to_string();
    if header != [b"P6".as_slice(), w.as_bytes(), h.as_bytes(), b"255".as_slice()] {
        return Err(true);
    }
    let expected = width * height * 3;
    let pixels = &data[cursor..data.len().min(cursor + expected + 1)];
    if pixels.len() != expected {
        return Err(false);
    }
    Ok(pixels)
}

pub enum FilteredError {
    Invalid,
    Truncated,
    Crc,
    Header,
    End,
    Chunk,
    Format,
    Corrupt,
    Size,
    Filter,
}

impl FilteredError {
    /// ink_render._rgba_png messages (InkRenderError).
    pub fn message(&self) -> &'static str {
        match self {
            FilteredError::Invalid => "Geçersiz çizim görüntüsü.",
            FilteredError::Truncated => "Eksik çizim görüntüsü.",
            FilteredError::Crc => "Görüntü bütünlüğü doğrulanamadı.",
            FilteredError::Header => "Geçersiz görüntü başlığı.",
            FilteredError::End => "Geçersiz görüntü sonu.",
            FilteredError::Chunk => "Desteklenmeyen görüntü parçası.",
            FilteredError::Format => "Desteklenmeyen görüntü biçimi.",
            FilteredError::Corrupt => "Error -3 while decompressing data",
            FilteredError::Size => "Görüntü boyutu doğrulanamadı.",
            FilteredError::Filter => "Geçersiz görüntü filtresi.",
        }
    }
}

/// ink_render._rgba_png without the final encode: any-filter RGB/RGBA8
/// noninterlaced PNG to straight RGBA bytes.
pub fn unfilter_png(raw: &[u8], width: i64, height: i64, max_png: usize) -> Result<Vec<u8>, FilteredError> {
    if raw.len() > max_png || raw.len() < 8 || &raw[..8] != SIGNATURE {
        return Err(FilteredError::Invalid);
    }
    let mut pos = 8;
    let mut packed = Vec::new();
    let mut header: Option<[u8; 13]> = None;
    let mut ended = false;
    while pos + 12 <= raw.len() {
        let size = be32(raw, pos) as usize;
        let kind = &raw[pos + 4..pos + 8];
        let end = pos + 12 + size;
        if end > raw.len() {
            return Err(FilteredError::Truncated);
        }
        let data = &raw[pos + 8..pos + 8 + size];
        if crc32(&[kind, data]) != be32(raw, pos + 8 + size) {
            return Err(FilteredError::Crc);
        }
        if kind == b"IHDR" {
            if header.is_some() || pos != 8 || size != 13 {
                return Err(FilteredError::Header);
            }
            let mut value = [0u8; 13];
            value.copy_from_slice(data);
            header = Some(value);
        } else if kind == b"IDAT" {
            packed.extend_from_slice(data);
        } else if kind == b"IEND" {
            if size != 0 || end != raw.len() {
                return Err(FilteredError::End);
            }
            ended = true;
            break;
        } else if kind == b"tRNS" || kind[0].is_ascii_uppercase() {
            return Err(FilteredError::Chunk);
        }
        pos = end;
    }
    let area = width as i128 * height as i128;
    let header = match header {
        Some(h) if ended => h,
        _ => return Err(FilteredError::Format),
    };
    if be32(&header, 0) as i64 != width || be32(&header, 4) as i64 != height || header[8] != 8
        || (header[9] != 2 && header[9] != 6) || header[10..] != [0, 0, 0]
        || !(0 < area && area <= MAX_PIXELS as i128)
    {
        return Err(FilteredError::Format);
    }
    let (w, h) = (width as usize, height as usize);
    let bpp = if header[9] == 2 { 3 } else { 4 };
    let stride = w * bpp;
    let data = match inflate_exact(&packed, h * (stride + 1)) {
        Inflate::Ok(data) => data,
        Inflate::Corrupt => return Err(FilteredError::Corrupt),
        Inflate::Size => return Err(FilteredError::Size),
    };
    let mut out = vec![0u8; w * h * 4];
    let mut previous = vec![0u8; stride];
    let mut row = vec![0u8; stride];
    for y in 0..h {
        let line = &data[y * (stride + 1)..(y + 1) * (stride + 1)];
        let method = line[0];
        row.copy_from_slice(&line[1..]);
        match method {
            0 => {}
            1 => {
                for i in bpp..stride {
                    row[i] = row[i].wrapping_add(row[i - bpp]);
                }
            }
            2 => {
                for i in 0..stride {
                    row[i] = row[i].wrapping_add(previous[i]);
                }
            }
            3 => {
                for i in 0..stride {
                    let left = if i >= bpp { row[i - bpp] as u16 } else { 0 };
                    row[i] = row[i].wrapping_add(((left + previous[i] as u16) / 2) as u8);
                }
            }
            4 => {
                for i in 0..stride {
                    let left = if i >= bpp { row[i - bpp] as i16 } else { 0 };
                    let up = previous[i] as i16;
                    let corner = if i >= bpp { previous[i - bpp] as i16 } else { 0 };
                    let value = left + up - corner;
                    let (a, b, c) = ((value - left).abs(), (value - up).abs(), (value - corner).abs());
                    let predict = if a <= b && a <= c { left } else if b <= c { up } else { corner };
                    row[i] = row[i].wrapping_add(predict as u8);
                }
            }
            _ => return Err(FilteredError::Filter),
        }
        let target = &mut out[y * w * 4..(y + 1) * w * 4];
        if bpp == 4 {
            target.copy_from_slice(&row);
        } else {
            for (dst, src) in target.chunks_exact_mut(4).zip(row.chunks_exact(3)) {
                dst[..3].copy_from_slice(src);
                dst[3] = 255;
            }
        }
        std::mem::swap(&mut previous, &mut row);
    }
    Ok(out)
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_validate() {
        let pixels: Vec<u8> = (0..(5 * 3 * 4)).map(|i| (i * 7) as u8).collect();
        let png = rgba_png(5, 3, &pixels).unwrap();
        assert!(png_valid(&png, 5, 3));
        assert!(!png_valid(&png, 3, 5));
        assert_eq!(rgba(&png, 5, 3).ok().unwrap(), pixels);
        assert_eq!(unfilter_png(&png, 5, 3, 1 << 25).ok().unwrap(), pixels);
    }
}
