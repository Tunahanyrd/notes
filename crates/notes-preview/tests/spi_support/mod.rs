//! Test-only SPI stream constructor, written from docs/CANVAS_CACHE.md.
//!
//! It builds synthetic canvas-cache images block by block from random but
//! valid choices (seeded), so fixtures exercise every coded path: copies,
//! literals, palettes, differential planes (full and reduced), intra planes
//! with all 18 prediction modes, both quantizer paths, every partition
//! layout, alpha literals and multiple packets. No real note data is used.
#![allow(dead_code)]

pub struct BitWriter {
    pub bytes: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter { bytes: Vec::new(), bit: 0 }
    }
    pub fn put(&mut self, value: u64, n: u32) {
        for i in (0..n).rev() {
            if self.bit % 8 == 0 {
                self.bytes.push(0);
            }
            if (value >> i) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bit % 8);
            }
            self.bit += 1;
        }
    }
    pub fn flag(&mut self, b: bool) {
        self.put(b as u64, 1);
    }
    /// `k` zeros, a one, `k` suffix bits for `value = 2^k + suffix` (value >= 1).
    pub fn positive(&mut self, value: u64) {
        assert!(value >= 1);
        let k = 63 - value.leading_zeros();
        self.put(0, k);
        self.put(1, 1);
        self.put(value - (1 << k), k);
    }
    pub fn unary(&mut self, zeros: u32) {
        self.put(0, zeros);
        self.put(1, 1);
    }
    pub fn align(&mut self) {
        self.bit = (self.bit + 7) & !7;
    }
    /// Fill the rest of the current byte with arbitrary bits.
    pub fn pad(&mut self, bits: u64) {
        let n = ((8 - self.bit % 8) % 8) as u32;
        self.put(bits & ((1 << n) - 1), n);
    }
    pub fn raw(&mut self, data: &[u8]) {
        assert!(self.bit % 8 == 0);
        self.bytes.extend_from_slice(data);
        self.bit += 8 * data.len();
    }
}

/// xorshift64* — deterministic, dependency-free.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    pub fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

const LEVEL_RUN: [(u8, u8); 127] = [
    (1, 0), (1, 1), (2, 0), (1, 2), (3, 0), (1, 3), (4, 0), (1, 4), (2, 1), (1, 5), (5, 0), (1, 6), (6, 0), (1, 7),
    (3, 1), (2, 2), (1, 8), (7, 0), (1, 9), (8, 0), (1, 10), (9, 0), (4, 1), (1, 11), (2, 3), (1, 12), (10, 0),
    (1, 13), (3, 2), (11, 0), (1, 14), (5, 1), (2, 4), (12, 0), (1, 15), (6, 1), (13, 0), (2, 5), (1, 16), (14, 0),
    (1, 17), (4, 2), (3, 3), (7, 1), (15, 0), (1, 18), (2, 6), (1, 20), (1, 19), (16, 0), (1, 21), (8, 1), (17, 0),
    (1, 24), (2, 7), (1, 22), (5, 2), (1, 25), (3, 4), (18, 0), (9, 1), (1, 23), (2, 8), (19, 0), (4, 3), (10, 1),
    (1, 26), (20, 0), (6, 2), (2, 9), (21, 0), (3, 5), (11, 1), (1, 27), (22, 0), (2, 10), (7, 2), (1, 31), (12, 1),
    (1, 30), (23, 0), (1, 28), (1, 29), (5, 3), (4, 4), (1, 32), (2, 11), (24, 0), (13, 1), (8, 2), (3, 6), (25, 0),
    (2, 13), (2, 12), (14, 1), (26, 0), (1, 34), (1, 33), (6, 3), (15, 1), (27, 0), (4, 5), (1, 35), (2, 14), (9, 2),
    (5, 4), (28, 0), (3, 7), (16, 1), (29, 0), (10, 2), (17, 1), (30, 0), (7, 3), (3, 8), (31, 0), (18, 1), (1, 36),
    (11, 2), (32, 0), (1, 37), (3, 9), (33, 0), (2, 15), (19, 1), (6, 4), (34, 0),
];

fn run_of(code: usize) -> usize {
    LEVEL_RUN[code - 1].1 as usize
}

const MASK4_ROW0: [u8; 64] = [
    63, 60, 62, 61, 28, 52, 44, 56, 31, 47, 55, 59, 12, 48, 30, 54, 46, 58, 20, 29, 53, 45, 40, 57, 51, 15, 0, 16, 4,
    14, 50, 32, 8, 13, 49, 36, 24, 22, 43, 23, 42, 21, 41, 38, 34, 26, 18, 6, 39, 10, 2, 33, 17, 27, 5, 37, 35, 1, 25,
    9, 19, 7, 11, 3,
];
const MASK4_ROWS: [[u8; 64]; 5] = [
    MASK4_ROW0,
    [60, 63, 62, 61, 28, 52, 44, 56, 12, 48, 20, 40, 31, 47, 59, 16, 55, 4, 32, 8, 30, 54, 46, 58, 29, 0, 36, 53, 45,
     24, 57, 15, 51, 50, 14, 13, 49, 22, 43, 42, 23, 21, 41, 34, 18, 38, 10, 6, 26, 17, 33, 39, 9, 5, 35, 37, 27, 25,
     19, 11, 2, 7, 1, 3],
    [60, 63, 61, 62, 28, 12, 52, 44, 56, 48, 20, 40, 4, 16, 8, 32, 0, 31, 59, 47, 55, 36, 24, 29, 45, 53, 57, 30, 15,
     58, 46, 54, 13, 51, 49, 50, 14, 43, 21, 41, 23, 42, 22, 33, 34, 17, 9, 5, 35, 18, 10, 6, 11, 39, 37, 27, 25, 38,
     19, 26, 7, 1, 2, 3],
    [60, 28, 12, 52, 44, 56, 63, 20, 48, 40, 4, 16, 8, 61, 32, 62, 0, 36, 24, 31, 59, 29, 47, 55, 57, 53, 45, 13, 30,
     15, 58, 49, 51, 46, 54, 21, 14, 50, 43, 41, 23, 42, 22, 33, 9, 17, 35, 5, 34, 11, 10, 37, 25, 18, 27, 39, 6, 19,
     1, 38, 7, 26, 2, 3],
    [60, 12, 28, 20, 4, 48, 44, 52, 56, 40, 8, 16, 0, 32, 36, 24, 63, 61, 62, 31, 59, 29, 57, 47, 13, 55, 45, 53, 49,
     51, 15, 21, 41, 30, 58, 43, 46, 54, 23, 50, 14, 33, 9, 42, 22, 17, 35, 5, 34, 11, 25, 37, 1, 27, 10, 18, 19, 6,
     39, 7, 2, 26, 38, 3],
];
const MASK_LARGE: [u8; 8] = [4, 0, 6, 7, 5, 2, 1, 3];

fn mask4_row(q: u8) -> usize {
    match q {
        22..=26 => 1,
        27..=31 => 2,
        32..=36 => 3,
        37..=50 => 4,
        _ => 0,
    }
}

/// Knobs for one constructed image.
#[derive(Clone)]
pub struct Plan {
    pub width: usize,
    pub height: usize,
    pub row_group: usize,
    pub seed: u64,
    /// Percent weights for colour modes 0..=5 and alpha modes 0,1,3,5.
    pub color_weights: [u64; 6],
    pub alpha_weights: [u64; 4],
    /// Candidate quantizers for packet bytes C/D.
    pub quantizers: Vec<u8>,
    /// Restrict intra prediction modes (empty = all 18).
    pub modes: Vec<u8>,
    /// Allow the reduced differential path.
    pub reduced: bool,
}

/// Marker cells of the current block row for one plane (4 rows x 4C).
struct Cells {
    width: usize,
    v: Vec<u8>,
}

impl Cells {
    fn new(columns: usize) -> Self {
        Cells { width: 4 * columns, v: vec![2; 16 * columns] }
    }
    fn left_of(&self, cx: usize, cy: usize) -> u8 {
        if cx == 0 { 2 } else { self.v[cy * self.width + cx - 1] }
    }
    fn set(&mut self, cx: usize, cy: usize, value: u8) {
        self.v[cy * self.width + cx] = value;
    }
    fn reset(&mut self, cx: usize) {
        for cy in 0..4 {
            for dx in 0..4 {
                self.set(cx + dx, cy, 2);
            }
        }
    }
}

struct PaletteModel {
    count: usize,
    width: u32,
    cached: [bool; 256],
}

pub fn build(plan: &Plan) -> Vec<u8> {
    let mut rng = Rng::new(plan.seed);
    let columns = plan.width.div_ceil(16);
    let rows = plan.height.div_ceil(16);
    let mut cells: Vec<Cells> = (0..4).map(|_| Cells::new(columns)).collect();
    let mut palette = PaletteModel { count: 0, width: 0, cached: [false; 256] };
    let mut data = Vec::new();
    let groups = rows.div_ceil(plan.row_group);
    for group in 0..groups {
        let c = plan.quantizers[rng.below(plan.quantizers.len() as u64) as usize];
        let d = plan.quantizers[rng.below(plan.quantizers.len() as u64) as usize];
        let selector = rng.below(4) as u8;
        let mut w = BitWriter::new();
        w.put(0xaa, 8);
        w.put(2, 8);
        w.put(0, 32);
        w.put(0, 8);
        w.put(group as u64, 16);
        w.put(0, 8);
        w.put(0, 1);
        w.put(selector as u64, 2);
        w.put(c as u64, 8);
        w.put(d as u64, 8);
        w.put(rng.byte() as u64, 8);
        w.put(0, 5);
        palette.count = 0;
        palette.width = 0;
        let first = group * plan.row_group;
        let last = (first + plan.row_group).min(rows);
        for by in first..last {
            for bx in 0..columns {
                color_block(&mut w, &mut rng, plan, &mut cells, &mut palette, bx, by, columns, rows, (c, d), selector);
            }
        }
        w.align();
        for by in first..last {
            for bx in 0..columns {
                alpha_block(&mut w, &mut rng, plan, &mut cells[3], bx, by, columns, rows);
            }
        }
        w.align();
        let length = w.bytes.len() as u32;
        w.bytes[2..6].copy_from_slice(&length.to_be_bytes());
        data.extend_from_slice(&w.bytes);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&[0xaa, 1, 0, 0, 0, 20, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&(plan.width as u16).to_be_bytes());
    out.extend_from_slice(&(plan.height as u16).to_be_bytes());
    out.push(4);
    out.extend_from_slice(&(plan.row_group as u16).to_be_bytes());
    out.push(0);
    out.push(0xe0);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn pick(rng: &mut Rng, weights: &[u64]) -> usize {
    let total: u64 = weights.iter().sum();
    let mut r = rng.below(total);
    for (i, &w) in weights.iter().enumerate() {
        if r < w {
            return i;
        }
        r -= w;
    }
    weights.len() - 1
}

fn copy_block(w: &mut BitWriter, rng: &mut Rng, bx: usize, by: usize, columns: usize, rows: usize) -> bool {
    // Returns false if no copy source exists (top-left block).
    if bx == 0 && by == 0 {
        return false;
    }
    if rng.chance(40) {
        w.put(1, 1);
        return true;
    }
    w.put(0b01, 2);
    if by > 0 && rng.chance(30) {
        w.put(1, 1);
        return true;
    }
    loop {
        let hb = rng.below(5) as i64 - 2;
        let vb = rng.below(3) as i64;
        let (sx, sy) = (bx as i64 - hb, by as i64 - vb);
        if sx >= 0 && sy >= 0 && (sx as usize) < columns && (sy as usize) < rows && (hb, vb) != (0, 0) {
            w.put(0, 1);
            w.positive(if hb > 0 { 2 * hb as u64 } else { (1 - 2 * hb) as u64 });
            w.positive(vb as u64 + 1);
            return true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn color_block(w: &mut BitWriter, rng: &mut Rng, plan: &Plan, cells: &mut [Cells], palette: &mut PaletteModel,
               bx: usize, by: usize, columns: usize, rows: usize, (c, d): (u8, u8), selector: u8) {
    let mut mode = pick(rng, &plan.color_weights);
    if mode <= 1 && bx == 0 && by == 0 {
        mode = 5;
    }
    match mode {
        0 | 1 => {
            copy_block(w, rng, bx, by, columns, rows);
        }
        2 => {
            w.put(0b0000, 4);
            let reduced = plan.reduced && rng.chance(50);
            w.flag(reduced);
            let limit: u64 = match selector { 1 => 129, 2 => 65, _ => 256 };
            for p in 0..3 {
                let count = if reduced && p > 0 { 64 } else { 256 };
                symbols(w, rng, count, limit);
            }
        }
        3 => {
            let use_min = rng.chance(50);
            w.put(0b0001, 4);
            w.flag(use_min);
            let q = if use_min { c.min(d) } else { c };
            for p in 0..3 {
                intra_plane(w, rng, plan, &mut cells[p], bx, q, false);
            }
            return;
        }
        4 => {
            w.put(0b0010, 4);
            palette_payload(w, rng, palette);
        }
        _ => {
            w.put(0b0011, 4);
            w.pad(rng.next());
            let mut planes = vec![0u8; 768];
            let base = rng.byte();
            for (i, v) in planes.iter_mut().enumerate() {
                *v = if rng.chance(70) { base.wrapping_add((i % 16) as u8 * 3) } else { rng.byte() };
            }
            w.raw(&planes);
        }
    }
    for cell in cells[..3].iter_mut() {
        cell.reset(4 * bx);
    }
}

#[allow(clippy::too_many_arguments)]
fn alpha_block(w: &mut BitWriter, rng: &mut Rng, plan: &Plan, cells: &mut Cells, bx: usize, by: usize,
               columns: usize, rows: usize) {
    let mut choice = pick(rng, &plan.alpha_weights);
    if choice <= 1 && bx == 0 && by == 0 {
        choice = 2;
    }
    if choice == 3 && 4 * bx + 1 > columns {
        choice = 2;
    }
    match choice {
        0 | 1 => {
            copy_block(w, rng, bx, by, columns, rows);
            cells.reset(4 * bx);
        }
        2 => {
            w.put(0b0001, 4);
            intra_plane(w, rng, plan, cells, bx, 0, true);
        }
        _ => {
            w.put(0b0011, 4);
            w.pad(rng.next());
            let mut plane = vec![0u8; 256];
            for (i, v) in plane.iter_mut().enumerate() {
                *v = if rng.chance(50) { 255 } else { (i * 7) as u8 };
            }
            w.raw(&plane);
            // Unscaled reset as the codec does it.
            cells.reset(16 * bx);
        }
    }
}

fn symbols(w: &mut BitWriter, rng: &mut Rng, count: usize, limit: u64) {
    let (mut older, mut newer) = (255u8, 0u8);
    let mut written = 0usize;
    while written < count {
        if older == newer {
            let room = count - written - 1;
            let repeat = if room > 0 && rng.chance(50) { rng.below(room.min(40) as u64 + 1) } else { 0 };
            w.positive(repeat + 1);
            written += repeat as usize;
        }
        let symbol = if rng.chance(60) { rng.below(4) as u8 * 2 } else { rng.below(limit) as u8 };
        if symbol <= 83 && rng.chance(90) {
            w.unary(symbol as u32 / 4);
            w.put(symbol as u64 % 4, 2);
        } else if (84..=86).contains(&symbol) {
            w.unary(21);
            w.put(symbol as u64 - 84, 2);
        } else {
            w.unary(21);
            w.put(3, 2);
            w.put(symbol as u64, 8);
        }
        written += 1;
        older = newer;
        newer = symbol;
    }
}

fn palette_payload(w: &mut BitWriter, rng: &mut Rng, pal: &mut PaletteModel) {
    let op = rng.below(4);
    let can_reuse = pal.width > 0 || pal.cached[0];
    if op == 0 && can_reuse {
        w.put(0, 1);
        if pal.width == 0 {
            return;
        }
        runs(w, rng, pal.width, pal.count);
        return;
    }
    if op == 1 && pal.count > 0 && pal.count < 200 {
        // Extend.
        w.put(0b10, 2);
        let count = pal.count + rng.below(4) as usize;
        let width = (usize::BITS - (count - 1).leading_zeros()).max(1);
        w.put(width as u64, 4);
        w.put(count as u64 - 1, width);
        for i in pal.count..count {
            w.put(rng.next() & 0xff_ffff, 24);
            pal.cached[i] = true;
        }
        runs(w, rng, width, count);
        pal.count = count;
        pal.width = width;
        return;
    }
    // Replace (optionally with a single colour).
    w.put(0b11, 2);
    if rng.chance(20) {
        w.put(0, 4);
        w.put(rng.next() & 0xff_ffff, 24);
        pal.cached = [false; 256];
        pal.cached[0] = true;
        pal.count = 1;
        pal.width = 0;
        return;
    }
    let count = 1 + rng.below(12) as usize;
    let width = (usize::BITS - (count - 1).leading_zeros()).max(1) + rng.below(2) as u32;
    w.put(width as u64, 4);
    w.put(count as u64 - 1, width);
    for _ in 0..count {
        w.put(rng.next() & 0xff_ffff, 24);
    }
    runs(w, rng, width, count);
    pal.cached = [false; 256];
    pal.cached[..count].fill(true);
    pal.count = count;
    pal.width = width;
}

fn runs(w: &mut BitWriter, rng: &mut Rng, width: u32, count: usize) {
    let mut filled = 0usize;
    while filled < 256 {
        let run = 1 + rng.below((256 - filled).min(48) as u64) as usize;
        w.put(rng.below(count as u64), width);
        w.positive(run as u64);
        filled += run;
    }
}

fn intra_plane(w: &mut BitWriter, rng: &mut Rng, plan: &Plan, cells: &mut Cells, bx: usize, q: u8, alpha: bool) {
    let divided = rng.chance(70);
    w.flag(divided);
    let sections: &[(usize, usize)] = if divided { &[(0, 0), (8, 0), (0, 8), (8, 8)] } else { &[(0, 0)] };
    let section_side = if divided { 8 } else { 16 };
    for &(sx, sy) in sections {
        let split = divided && rng.chance(40);
        if divided {
            w.flag(split);
        }
        let side = section_side >> split as usize;
        let positions: Vec<(usize, usize)> = if split {
            vec![(sx, sy), (sx + side, sy), (sx, sy + side), (sx + side, sy + side)]
        } else {
            vec![(sx, sy)]
        };
        let mut modes = Vec::new();
        for &(x, y) in &positions {
            let mode = if plan.modes.is_empty() {
                rng.below(18) as u8
            } else {
                plan.modes[rng.below(plan.modes.len() as u64) as usize]
            };
            let cx = 4 * bx + x / 4;
            let cy = y / 4;
            let left = match cells.left_of(cx, cy) { 17 => 2, m => m };
            if mode == 2 || mode == 17 {
                w.put(0b01, 2);
                w.flag(mode == 17);
            } else if mode == left {
                // "1" prefix with matching neighbours is not used: the above
                // marker is not tracked here, so code the left mode via the
                // selector form only when it is unambiguous. Fall back to 2.
                w.put(0b01, 2);
                w.flag(false);
                modes.push(2);
                let n = side / 4;
                for dy in 0..n {
                    for dx in 0..n {
                        cells.set(cx + dx, cy + dy, 2);
                    }
                }
                continue;
            } else {
                let candidates: Vec<u8> = (0u8..18).filter(|&m| m != left && m != 2).collect();
                let index = candidates.iter().position(|&m| m == mode).unwrap();
                if index > 15 {
                    w.put(0b01, 2);
                    w.flag(true);
                    modes.push(17);
                    let n = side / 4;
                    for dy in 0..n {
                        for dx in 0..n {
                            cells.set(cx + dx, cy + dy, 17);
                        }
                    }
                    continue;
                }
                w.put(0b00, 2);
                w.put(index as u64, 4);
            }
            modes.push(mode);
            let n = side / 4;
            for dy in 0..n {
                for dx in 0..n {
                    cells.set(cx + dx, cy + dy, mode);
                }
            }
        }
        let count = positions.len();
        let mask = if side == 4 {
            let code = 1 + rng.below(64);
            w.positive(code);
            MASK4_ROWS[mask4_row(q)][code as usize - 1]
        } else {
            let k = rng.below(8) as u32;
            w.unary(k);
            MASK_LARGE[k as usize]
        };
        for (index, _) in positions.iter().enumerate() {
            if mask >> (count + 1 - index) & 1 == 1 {
                if q == 0 {
                    lossless_tokens(w, rng, side, alpha);
                } else {
                    quantized_tokens(w, rng, side);
                }
            }
        }
    }
}

fn lossless_tokens(w: &mut BitWriter, rng: &mut Rng, side: usize, alpha: bool) {
    let area = side * side;
    let tokens = 1 + rng.below(area.min(if alpha { 10 } else { 6 }) as u64) as usize;
    w.positive(tokens as u64);
    let mut next = 0usize;
    for t in 0..tokens {
        let left_after = tokens - t - 1;
        let room = area - next - left_after;
        // Pick a compact code whose run fits, else an escape.
        let code = loop {
            if rng.chance(75) {
                let code = 1 + rng.below(40) as usize;
                if run_of(code) < room {
                    break code as u64;
                }
            } else {
                let run = rng.below(room as u64) as usize;
                let level = 1 + rng.below(if alpha { 60 } else { 30 }) as usize;
                break (128 + (level - 1) * area + run) as u64;
            }
        };
        let run = if code < 128 { run_of(code as usize) } else { (code as usize - 128) % area };
        w.positive(code);
        w.flag(rng.chance(50));
        next += run + 1;
    }
}

fn quantized_tokens(w: &mut BitWriter, rng: &mut Rng, side: usize) {
    let area = side * side;
    let tokens = 1 + rng.below(area.min(8) as u64) as usize;
    w.positive(tokens as u64);
    let mut next = 0usize;
    for t in 0..tokens {
        let left_after = tokens - t - 1;
        let room = area - next - left_after;
        let code = loop {
            if rng.chance(75) {
                let code = 1 + rng.below(40) as usize;
                if run_of(code) < room {
                    break code as u64;
                }
            } else {
                let run = rng.below(room.min(256) as u64) as usize;
                let level = 1 + rng.below(40) as usize;
                break (256 * (level + 1) + run + 1) as u64;
            }
        };
        let run = if code < 128 { run_of(code as usize) } else { ((code - 1) & 255) as usize };
        w.positive(code);
        w.flag(rng.chance(50));
        next += run + 1;
    }
}
