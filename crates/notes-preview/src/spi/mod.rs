//! Independent decoder for Samsung Notes canvas-cache images (`.spi`).
//!
//! Written from our own description of the format in docs/CANVAS_CACHE.md.
//! Supported: the configuration Samsung Notes uses for saved page caches —
//! wire colour index 4 (three colour planes plus alpha), header flags `0xe0`,
//! no reference images, data packets with byte B = 0 and no buffer-copy
//! shortcut, block modes 0–5 for colour and 0/1/3/5 for alpha. Anything else
//! is rejected as unsupported; malformed input is rejected, never guessed.
//!
//! The decoder allocates only after the header has been bounded, never
//! indexes outside its buffers on hostile input, and either returns the
//! complete image or an error (no partial images).
mod bits;
mod intra;
mod tables;

use bits::Bits;
use intra::{clip_byte, wrap16, Recon};

/// Largest image (pixels) the decoder accepts.
pub const MAX_PIXELS: usize = 4 * 1024 * 1024;
/// Largest input accepted (bytes).
pub const MAX_INPUT: usize = 32 * 1024 * 1024;
const MAX_DIMENSION: usize = 32768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiError {
    /// The input ended inside a field.
    Truncated,
    /// Well-formed but outside the supported configuration.
    Unsupported(&'static str),
    /// Violates the format.
    Invalid(&'static str),
    /// Exceeds the pixel budget.
    TooLarge,
}

impl std::fmt::Display for SpiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpiError::Truncated => write!(f, "truncated SPI data"),
            SpiError::Unsupported(m) => write!(f, "unsupported SPI variant: {m}"),
            SpiError::Invalid(m) => write!(f, "invalid SPI data: {m}"),
            SpiError::TooLarge => write!(f, "SPI image exceeds the pixel budget"),
        }
    }
}

impl std::error::Error for SpiError {}

/// Parsed 20-byte image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub width: usize,
    pub height: usize,
    pub color_index: u8,
    pub row_group: usize,
    pub reference_images: u8,
    pub flags: u8,
}

/// A decoded image: `width * height` pixels of four bytes in the codec's
/// output order (colour plane 2, plane 0, plane 1, alpha), straight alpha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

fn le32(data: &[u8], at: usize) -> Result<usize, SpiError> {
    let b = data.get(at..at + 4).ok_or(SpiError::Truncated)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Split the outer framing and parse the header. Returns the header and
/// the encoded data block.
pub fn parse_header(data: &[u8]) -> Result<(Header, &[u8]), SpiError> {
    if data.len() > MAX_INPUT {
        return Err(SpiError::TooLarge);
    }
    if le32(data, 0)? != 20 {
        return Err(SpiError::Invalid("header block length"));
    }
    let h = data.get(4..24).ok_or(SpiError::Truncated)?;
    if h[0] != 0xaa || h[1] != 1 {
        return Err(SpiError::Invalid("header packet marker"));
    }
    if u32::from_be_bytes([h[2], h[3], h[4], h[5]]) != 20 {
        return Err(SpiError::Invalid("header packet length"));
    }
    let be16 = |o: usize| u16::from_be_bytes([h[o], h[o + 1]]) as usize;
    let (cap_w, cap_h, width, height) = (be16(7), be16(9), be16(11), be16(13));
    if width == 0 || height == 0 || width.max(height).max(cap_w).max(cap_h) > MAX_DIMENSION {
        return Err(SpiError::Invalid("image dimensions"));
    }
    if cap_w != 0 && cap_h != 0 && (cap_w < width || cap_h < height) {
        return Err(SpiError::Invalid("capacity smaller than image"));
    }
    let row_group = be16(16);
    if row_group == 0 || row_group > height.div_ceil(16) {
        return Err(SpiError::Invalid("row group size"));
    }
    if h[19] & 0x0f != 0 {
        return Err(SpiError::Invalid("reserved header flags"));
    }
    let header = Header { width, height, color_index: h[15], row_group, reference_images: h[18], flags: h[19] };
    let length = le32(data, 24)?;
    if length == 0 || data.len() - 28 != length {
        return Err(SpiError::Invalid("data block length"));
    }
    Ok((header, &data[28..]))
}

/// Prediction-mode markers of one plane: one byte per 4x4 cell, nine rows
/// of `4 * columns + 1` bytes (guard row, previous block row, current block
/// row; the first column of each row is a left sentinel).
pub(super) struct Markers {
    buf: Vec<u8>,
    stride: usize,
}

impl Markers {
    fn new(columns: usize) -> Self {
        let stride = 4 * columns + 1;
        Markers { buf: vec![2; 9 * stride], stride }
    }
    fn base(&self) -> usize {
        5 * self.stride + 1
    }
    /// Cell `(cx, cy)` of the current block row; `cx` and `cy` may be -1.
    pub(super) fn at(&self, cx: isize, cy: isize) -> u8 {
        let index = self.base() as isize + cy * self.stride as isize + cx;
        self.buf[index as usize]
    }
    pub(super) fn set(&mut self, cx: usize, cy: usize, value: u8) {
        let index = self.base() + cy * self.stride + cx;
        self.buf[index] = value;
    }
    /// Reset a 4x4-cell rectangle starting `offset` cells right of the
    /// current row's first cell. The literal alpha path passes an offset
    /// that can reach past the buffer; that is rejected.
    fn reset(&mut self, offset: usize) -> Result<(), SpiError> {
        let start = self.base() + offset;
        if start + 3 * self.stride + 4 > self.buf.len() {
            return Err(SpiError::Unsupported("literal alpha block outside the marker area"));
        }
        for k in 0..4 {
            let at = start + k * self.stride;
            self.buf[at..at + 4].fill(2);
        }
        Ok(())
    }
    fn new_group(&mut self) {
        let s = self.stride;
        self.buf[s..5 * s].fill(2);
    }
    fn row_done(&mut self) {
        let s = self.stride;
        self.buf.copy_within(5 * s..9 * s, s);
    }
}

/// Palette state kept across blocks (count and width reset per packet).
struct Palette {
    count: usize,
    width: u32,
    cached: [[u8; 3]; 256],
    cached_set: [bool; 256],
    temporary: [[u8; 3]; 256],
    temporary_set: [bool; 256],
}

struct Decoder {
    width: usize,
    height: usize,
    columns: usize,
    rows: usize,
    stride: usize,
    /// Planes 0..2 colour, 3 alpha; 16-pixel border filled with 128.
    planes: [Vec<u8>; 4],
    markers: [Markers; 4],
    palette: Box<Palette>,
    /// Byte history preceding each colour plane for differential blocks.
    prefix: [[u8; 16]; 3],
    selector: u8,
    quant_c: u8,
    quant_d: u8,
    stats: Stats,
}

const BORDER: usize = 16;

impl Decoder {
    fn new(header: &Header) -> Self {
        let columns = header.width.div_ceil(16);
        let rows = header.height.div_ceil(16);
        let stride = 16 * columns + 2 * BORDER;
        let size = stride * (16 * rows + 2 * BORDER);
        Decoder {
            width: header.width,
            height: header.height,
            columns,
            rows,
            stride,
            planes: std::array::from_fn(|_| vec![128u8; size]),
            markers: std::array::from_fn(|_| Markers::new(columns)),
            palette: Box::new(Palette {
                count: 0,
                width: 0,
                cached: [[0; 3]; 256],
                cached_set: [false; 256],
                temporary: [[0; 3]; 256],
                temporary_set: [false; 256],
            }),
            prefix: [[128; 16]; 3],
            selector: 0,
            quant_c: 0,
            quant_d: 0,
            stats: Stats::default(),
        }
    }

    /// Frame index of image pixel `(x, y)` (both may be -16..).
    fn at(&self, x: isize, y: isize) -> usize {
        ((y + BORDER as isize) as usize) * self.stride + (x + BORDER as isize) as usize
    }

    fn store_block(&mut self, plane: usize, bx: usize, by: usize, block: &[u8; 256]) {
        for row in 0..16 {
            let at = self.at((bx * 16) as isize, (by * 16 + row) as isize);
            self.planes[plane][at..at + 16].copy_from_slice(&block[row * 16..row * 16 + 16]);
        }
    }

    fn copy_block(&mut self, plane: usize, from: (usize, usize), to: (usize, usize)) {
        if from == to {
            return;
        }
        for row in 0..16 {
            let src = self.at((from.0 * 16) as isize, (from.1 * 16 + row) as isize);
            let dst = self.at((to.0 * 16) as isize, (to.1 * 16 + row) as isize);
            self.planes[plane].copy_within(src..src + 16, dst);
        }
    }

    /// External edges of a block for one plane: raw gather plus completion
    /// for the neighbours that exist (every earlier block of the image).
    fn edges(&self, plane: usize, bx: usize, by: usize, group_start: usize) -> ([i32; 33], [i32; 33]) {
        let (x, y) = ((bx * 16) as isize, (by * 16) as isize);
        let p = &self.planes[plane];
        let mut above = [0i32; 33];
        let mut left = [0i32; 33];
        let has_above = by > group_start;
        if has_above {
            for (i, v) in above.iter_mut().enumerate() {
                *v = p[self.at(x - 1 + i as isize, y - 1)] as i32;
            }
            left[0] = above[0];
        } else {
            left[0] = 128;
        }
        for i in 1..=16 {
            left[i] = p[self.at(x - 1, y + i as isize - 1)] as i32;
        }
        let has_left = bx > 0;
        if !has_above {
            if (bx == 0 && by == 0) || !has_left {
                above = [128; 33];
                left = [128; 33];
            } else {
                for i in 17..33 {
                    left[i] = left[16];
                }
                above = [left[1]; 33];
            }
            return (left, above);
        }
        let above_right = bx + 1 < self.columns;
        let above_left = bx > 0;
        complete_edges(&mut left, &mut above, has_left, true, above_right, above_left);
        (left, above)
    }
}

/// Fill missing edge entries from the neighbours that exist.
fn complete_edges(l: &mut [i32; 33], a: &mut [i32; 33], left: bool, above: bool, above_right: bool,
                  above_left: bool) {
    let (l0, l1, l16, a0, a1, a16, a17) = (l[0], l[1], l[16], a[0], a[1], a[16], a[17]);
    let mean = |p: i32, q: i32| (p + q + 1) >> 1;
    let fill = |e: &mut [i32; 33], from: usize, to: usize, v: i32| e[from..=to].iter_mut().for_each(|x| *x = v);
    match (above_left, above_right, above, left) {
        (false, false, false, false) => {
            fill(a, 0, 32, 128);
            fill(l, 0, 32, 128);
        }
        (false, false, false, true) => {
            fill(a, 0, 32, l1);
            fill(l, 17, 32, l16);
        }
        (false, false, true, false) => {
            fill(a, 17, 32, a16);
            fill(l, 0, 32, a1);
        }
        (false, false, true, true) => {
            a[0] = mean(a1, l1);
            fill(a, 17, 32, a16);
            l[0] = mean(a1, l1);
            fill(l, 17, 32, l16);
        }
        (false, true, false, false) => {
            fill(a, 0, 16, a17);
            fill(l, 0, 32, a17);
        }
        (false, true, false, true) => {
            fill(a, 0, 16, mean(a17, l1));
            l[0] = mean(a17, l1);
            fill(l, 17, 32, l16);
        }
        (false, true, true, false) => {
            a[0] = a1;
            fill(l, 0, 32, a1);
        }
        (false, true, true, true) => {
            a[0] = mean(a1, l1);
            l[0] = mean(a1, l1);
            fill(l, 17, 32, l16);
        }
        (true, false, false, false) => {
            fill(a, 1, 32, a0);
            fill(l, 1, 32, l0);
        }
        (true, false, false, true) => {
            fill(a, 1, 32, a0);
            fill(l, 17, 32, l16);
        }
        (true, false, true, false) => {
            fill(a, 17, 32, a16);
            fill(l, 1, 32, l0);
        }
        (true, false, true, true) => {
            fill(a, 17, 32, a16);
            fill(l, 17, 32, l16);
        }
        (true, true, false, false) => {
            fill(a, 1, 16, mean(a0, a17));
            fill(l, 1, 32, l0);
        }
        (true, true, false, true) => {
            fill(a, 1, 16, mean(a0, a17));
            fill(l, 17, 32, l16);
        }
        (true, true, true, false) => fill(l, 1, 32, l0),
        (true, true, true, true) => fill(l, 17, 32, l16),
    }
}

/// Forward reversible colour transform of one byte triple.
fn to_ycc(a: i32, b: i32, c: i32) -> [i32; 3] {
    let d = c - b;
    let t = b + (d >> 1);
    let e = a - t;
    [t + (e >> 1), e + 256, d + 256]
}

/// Inverse colour transform back to bytes (planes 0, 1, 2).
fn from_ycc(y: i32, e: i32, d: i32) -> [u8; 3] {
    let e = wrap16(e) - 256;
    let d = wrap16(d) - 256;
    let t = wrap16(y - (e >> 1));
    let a = clip_byte(t + e);
    let b = clip_byte(t - (d >> 1));
    let c = clip_byte(b + d);
    [a as u8, b as u8, c as u8]
}

/// Block-mode counts gathered while decoding (diagnostics and tests).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub color_modes: [usize; 6],
    pub alpha_modes: [usize; 6],
    /// Intra partitions by prediction mode.
    pub prediction_modes: [usize; 18],
    /// Intra partitions by side 4/8/16, all planes.
    pub partition_sides: [usize; 3],
    /// Colour intra blocks with quantizer zero / nonzero.
    pub color_intra_q0: usize,
    pub color_intra_quantized: usize,
    /// Differential blocks with reduced secondary planes.
    pub differential_reduced: usize,
    pub packets: usize,
}

/// Decode a complete canvas-cache image.
pub fn decode(data: &[u8]) -> Result<Image, SpiError> {
    decode_with_stats(data).map(|(image, _)| image)
}

/// [`decode`] plus block-mode counts.
pub fn decode_with_stats(data: &[u8]) -> Result<(Image, Stats), SpiError> {
    let (header, body) = parse_header(data)?;
    if header.width * header.height > MAX_PIXELS {
        return Err(SpiError::TooLarge);
    }
    if header.color_index != 4 {
        return Err(SpiError::Unsupported("colour index"));
    }
    if header.flags != 0xe0 {
        return Err(SpiError::Unsupported("header flags"));
    }
    if header.reference_images != 0 {
        return Err(SpiError::Unsupported("reference images"));
    }
    let mut dec = Decoder::new(&header);
    let groups = dec.rows.div_ceil(header.row_group);
    let mut cursor = 0usize;
    for group in 0..groups {
        let rest = &body[cursor..];
        if rest.len() < 14 || rest[0] != 0xaa || rest[1] != 2 {
            return Err(SpiError::Invalid("missing data packet"));
        }
        let length = u32::from_be_bytes([rest[2], rest[3], rest[4], rest[5]]) as usize;
        if length < 14 || length > rest.len() {
            return Err(SpiError::Invalid("data packet length"));
        }
        let packet = &rest[..length];
        let mut bits = Bits::new(packet, 48);
        let _byte_a = bits.read(8)?;
        let index = bits.read(16)? as usize;
        let byte_b = bits.read(8)?;
        let shortcut = bits.bit()?;
        dec.selector = bits.read(2)? as u8;
        dec.quant_c = bits.read(8)? as u8;
        dec.quant_d = bits.read(8)? as u8;
        let _adjust = bits.read(8)?;
        if bits.read(5)? != 0 {
            return Err(SpiError::Invalid("reserved packet bits"));
        }
        if index != group {
            return Err(SpiError::Unsupported("packet order"));
        }
        if byte_b != 0 || shortcut {
            return Err(SpiError::Unsupported("packet configuration"));
        }
        let first = group * header.row_group;
        let last = (first + header.row_group).min(dec.rows);
        dec.palette.count = 0;
        dec.palette.width = 0;
        for m in dec.markers.iter_mut() {
            m.new_group();
        }
        for by in first..last {
            for bx in 0..dec.columns {
                dec.color_block(&mut bits, bx, by, first)?;
            }
            for m in dec.markers[..3].iter_mut() {
                m.row_done();
            }
        }
        bits.align();
        for by in first..last {
            for bx in 0..dec.columns {
                dec.alpha_block(&mut bits, bx, by, first)?;
            }
            dec.markers[3].row_done();
        }
        bits.align();
        if bits.position() != length * 8 {
            return Err(SpiError::Invalid("data packet size mismatch"));
        }
        cursor += length;
        dec.stats.packets += 1;
    }
    if cursor != body.len() {
        return Err(SpiError::Invalid("trailing data packets"));
    }
    let mut pixels = vec![0u8; header.width * header.height * 4];
    for y in 0..header.height {
        let row = dec.at(0, y as isize);
        for x in 0..header.width {
            let o = (y * header.width + x) * 4;
            pixels[o] = dec.planes[2][row + x];
            pixels[o + 1] = dec.planes[0][row + x];
            pixels[o + 2] = dec.planes[1][row + x];
            pixels[o + 3] = dec.planes[3][row + x];
        }
    }
    Ok((Image { width: dec.width, height: dec.height, pixels }, dec.stats))
}

fn read_mode_prefix(bits: &mut Bits) -> Result<u8, SpiError> {
    if bits.bit()? {
        return Ok(0);
    }
    if bits.bit()? {
        return Ok(1);
    }
    Ok(2 + bits.read(2)? as u8)
}

impl Decoder {
    /// Source block of a copy (modes 0 and 1); `None` when it would leave the image.
    fn copy_source(&self, bits: &mut Bits, mode: u8, bx: usize, by: usize) -> Result<(usize, usize), SpiError> {
        let (dx, dy): (i64, i64) = if mode == 0 {
            if bx > 0 { (1, 0) } else { (0, 1) }
        } else if bits.bit()? {
            (0, 1)
        } else {
            let h = bits.positive()? as i64;
            let v = bits.positive()? as i64;
            (if h % 2 == 0 { h / 2 } else { -(h / 2) }, v - 1)
        };
        let (sx, sy) = (bx as i64 - dx, by as i64 - dy);
        if sx < 0 || sy < 0 || sx >= self.columns as i64 || sy >= self.rows as i64 {
            return Err(SpiError::Invalid("copy source outside the image"));
        }
        Ok((sx as usize, sy as usize))
    }

    fn color_block(&mut self, bits: &mut Bits, bx: usize, by: usize, group_start: usize) -> Result<(), SpiError> {
        let mode = read_mode_prefix(bits)?;
        self.stats.color_modes[mode as usize] += 1;
        match mode {
            0 | 1 => {
                let source = self.copy_source(bits, mode, bx, by)?;
                for plane in 0..3 {
                    self.copy_block(plane, source, (bx, by));
                }
            }
            2 => self.differential_block(bits, bx, by, group_start)?,
            3 => {
                self.intra_color_block(bits, bx, by, group_start)?;
                return Ok(());
            }
            4 => {
                let planes = self.palette_block(bits)?;
                for (plane, block) in planes.iter().enumerate() {
                    self.store_block(plane, bx, by, block);
                }
            }
            _ => {
                bits.align();
                for plane in 0..3 {
                    let raw = bits.bytes(256)?;
                    let block: &[u8; 256] = raw.try_into().map_err(|_| SpiError::Truncated)?;
                    self.store_block(plane, bx, by, block);
                }
            }
        }
        for plane in 0..3 {
            self.markers[plane].reset(4 * bx)?;
        }
        Ok(())
    }

    fn alpha_block(&mut self, bits: &mut Bits, bx: usize, by: usize, group_start: usize) -> Result<(), SpiError> {
        let mode = read_mode_prefix(bits)?;
        self.stats.alpha_modes[mode as usize] += 1;
        match mode {
            0 | 1 => {
                let source = self.copy_source(bits, mode, bx, by)?;
                self.copy_block(3, source, (bx, by));
                self.markers[3].reset(4 * bx)?;
            }
            3 => {
                let (left, above) = self.edges(3, bx, by, group_start);
                let mut out = [0i32; 256];
                intra::decode_plane(bits, &mut self.markers[3], 4 * bx, Recon::Alpha, &left, &above, &mut out, &mut self.stats)?;
                let block: [u8; 256] = std::array::from_fn(|i| out[i] as u8);
                self.store_block(3, bx, by, &block);
            }
            5 => {
                bits.align();
                let raw = bits.bytes(256)?;
                let block: &[u8; 256] = raw.try_into().map_err(|_| SpiError::Truncated)?;
                self.store_block(3, bx, by, block);
                // The codec resets markers at the unscaled pixel column here.
                self.markers[3].reset(16 * bx)?;
            }
            _ => return Err(SpiError::Invalid("block mode not allowed for alpha")),
        }
        Ok(())
    }

    fn intra_color_block(&mut self, bits: &mut Bits, bx: usize, by: usize, group_start: usize)
                         -> Result<(), SpiError> {
        let q = if bits.bit()? { self.quant_c.min(self.quant_d) } else { self.quant_c };
        if q > 51 {
            return Err(SpiError::Unsupported("quantizer above 51"));
        }
        if q == 0 {
            self.stats.color_intra_q0 += 1;
        } else {
            self.stats.color_intra_quantized += 1;
        }
        let raw: [([i32; 33], [i32; 33]); 3] = std::array::from_fn(|p| self.edges(p, bx, by, group_start));
        let mut left = [[0i32; 33]; 3];
        let mut above = [[0i32; 33]; 3];
        for i in 0..33 {
            let l = to_ycc(raw[0].0[i], raw[1].0[i], raw[2].0[i]);
            let a = to_ycc(raw[0].1[i], raw[1].1[i], raw[2].1[i]);
            for p in 0..3 {
                left[p][i] = l[p];
                above[p][i] = a[p];
            }
        }
        let mut out = [[0i32; 256]; 3];
        for p in 0..3 {
            let recon = if q == 0 {
                Recon::ColorLossless
            } else {
                Recon::ColorQuantized { q, max: if p == 0 { 255 } else { 511 } }
            };
            intra::decode_plane(bits, &mut self.markers[p], 4 * bx, recon, &left[p], &above[p], &mut out[p],
                                &mut self.stats)?;
        }
        let mut blocks = [[0u8; 256]; 3];
        for i in 0..256 {
            let rgb = from_ycc(out[0][i], out[1][i], out[2][i]);
            for p in 0..3 {
                blocks[p][i] = rgb[p];
            }
        }
        for (p, block) in blocks.iter().enumerate() {
            self.store_block(p, bx, by, block);
        }
        Ok(())
    }

    fn palette_block(&mut self, bits: &mut Bits) -> Result<[[u8; 256]; 3], SpiError> {
        let pal = &mut *self.palette;
        let control = if bits.bit()? { if bits.bit()? { 2 } else { 1 } } else { 0 };
        let mut use_temporary = false;
        let (count, width);
        if control == 0 {
            count = pal.count;
            width = pal.width;
        } else {
            width = bits.read(4)?;
            if width == 0 {
                let triple = [bits.read(8)? as u8, bits.read(8)? as u8, bits.read(8)? as u8];
                pal.temporary[0] = triple;
                pal.temporary_set[0] = true;
                if control == 2 {
                    use_temporary = true;
                    count = 1;
                } else {
                    count = 0;
                }
            } else {
                count = bits.read(width)? as usize + 1;
                if count > 256 {
                    return Err(SpiError::Invalid("palette size"));
                }
                if control == 2 {
                    for i in 0..count {
                        pal.temporary[i] = [bits.read(8)? as u8, bits.read(8)? as u8, bits.read(8)? as u8];
                        pal.temporary_set[i] = true;
                    }
                    use_temporary = true;
                } else {
                    for i in pal.count..count {
                        pal.cached[i] = [bits.read(8)? as u8, bits.read(8)? as u8, bits.read(8)? as u8];
                        pal.cached_set[i] = true;
                    }
                }
            }
        }
        let mut indices = [0u8; 256];
        if width != 0 {
            let mut filled = 0usize;
            while filled < 256 {
                let index = (bits.read(width)? & 0xff) as u8;
                let run = bits.positive()? as usize;
                if run > 256 - filled {
                    return Err(SpiError::Invalid("palette run leaves block"));
                }
                indices[filled..filled + run].fill(index);
                filled += run;
            }
        }
        let (entries, set) = if use_temporary { (&pal.temporary, &pal.temporary_set) } else { (&pal.cached, &pal.cached_set) };
        let mut planes = [[0u8; 256]; 3];
        for (i, &index) in indices.iter().enumerate() {
            if !set[index as usize] {
                return Err(SpiError::Invalid("palette entry used before definition"));
            }
            let e = entries[index as usize];
            for p in 0..3 {
                planes[p][i] = e[p];
            }
        }
        if control == 2 {
            pal.cached = pal.temporary;
            pal.cached_set = pal.temporary_set;
        }
        pal.count = count;
        pal.width = width;
        Ok(planes)
    }

    fn differential_block(&mut self, bits: &mut Bits, bx: usize, by: usize, group_start: usize)
                          -> Result<(), SpiError> {
        let reduced = bits.bit()?;
        self.stats.differential_reduced += reduced as usize;
        let edges: [([i32; 33], [i32; 33]); 3] = std::array::from_fn(|p| self.edges(p, bx, by, group_start));
        if reduced {
            for i in 0..16 {
                self.prefix[0][i] = edges[0].1[1 + i] as u8;
            }
            for p in 1..3 {
                let a = &edges[p].1;
                let mut f = [0u8; 8];
                f[0] = ((3 * a[1] + a[2] + 2) >> 2) as u8;
                for (i, v) in f.iter_mut().enumerate().skip(1) {
                    *v = ((a[2 * i] + 2 * a[2 * i + 1] + a[2 * i + 2] + 2) >> 2) as u8;
                }
                self.prefix[p][8..16].copy_from_slice(&f);
            }
        } else {
            for p in 0..3 {
                for i in 0..16 {
                    self.prefix[p][i] = edges[p].1[1 + i] as u8;
                }
            }
        }
        let (step, range): (i32, i32) = match self.selector {
            1 => (2, 129),
            2 => (4, 65),
            _ => (1, 256),
        };
        let mut blocks = [[0u8; 256]; 3];
        for p in 0..3 {
            let small = reduced && p > 0;
            let count = if small { 64 } else { 256 };
            let symbols = read_symbols(bits, count)?;
            let mut plane = [0u8; 256];
            let distance = if small && self.selector != 0 { 8 } else { 16 };
            for i in 0..count {
                let previous = if i >= distance {
                    plane[i - distance] as i32
                } else {
                    self.prefix[p][16 - distance + i] as i32
                };
                let c = symbols[i] as i32;
                let d = if c % 2 == 0 { c / 2 } else { -(c + 1) / 2 };
                let mut v = previous + step * d;
                if self.selector == 0 {
                    v &= 255;
                } else {
                    let period = step * range;
                    if small {
                        v %= period;
                    }
                    let (low, high) = (-(step / 2), 255 + step / 2);
                    if v < low {
                        v += period;
                    } else if v > high {
                        v -= period;
                    }
                    if !(-128..=383).contains(&v) {
                        return Err(SpiError::Invalid("differential sample out of range"));
                    }
                    v = v.clamp(0, 255);
                }
                plane[i] = v as u8;
            }
            if small {
                plane = expand(&plane);
            }
            blocks[p] = plane;
        }
        for (p, block) in blocks.iter().enumerate() {
            self.store_block(p, bx, by, block);
        }
        Ok(())
    }
}

/// Byte symbols of a differential plane: unary/escape codes with repeats.
fn read_symbols(bits: &mut Bits, count: usize) -> Result<Vec<u8>, SpiError> {
    let mut out = Vec::with_capacity(count);
    let (mut older, mut newer) = (255u8, 0u8);
    while out.len() < count {
        if older == newer {
            let repeat = ((bits.positive()? - 1) & 255) as usize;
            if out.len() + repeat + 1 > count {
                return Err(SpiError::Invalid("differential repeat leaves plane"));
            }
            out.extend(std::iter::repeat_n(newer, repeat));
        }
        let q = bits.zeros(21)?;
        let r = bits.read(2)?;
        let symbol = if q < 21 {
            (4 * q + r) as u8
        } else if r <= 2 {
            (84 + r) as u8
        } else {
            bits.read(8)? as u8
        };
        out.push(symbol);
        older = newer;
        newer = symbol;
    }
    Ok(out)
}

/// 8x8 (stride 8, first 64 bytes) to 16x16 interpolation.
fn expand(small: &[u8; 256]) -> [u8; 256] {
    let px = |x: usize, y: usize| small[y * 8 + x] as i32;
    let mut tall = [[0i32; 8]; 16];
    for x in 0..8 {
        tall[0][x] = px(x, 0);
        for k in 0..7 {
            let (a, b) = (px(x, k), px(x, k + 1));
            tall[1 + 2 * k][x] = (3 * a + b + 2) >> 2;
            tall[2 + 2 * k][x] = (a + 3 * b + 2) >> 2;
        }
        tall[15][x] = px(x, 7);
    }
    let mut out = [0u8; 256];
    for (y, row) in tall.iter().enumerate() {
        for x in 0..8 {
            out[y * 16 + 2 * x] = row[x] as u8;
            out[y * 16 + 2 * x + 1] = if x < 7 { ((row[x] + row[x + 1] + 1) >> 1) as u8 } else { row[x] as u8 };
        }
    }
    out
}
