//! Intra-predicted blocks (block mode 3): sections, partition prediction,
//! coefficient decoding, dequantization, inverse transforms and residual
//! combination. Works on one plane at a time with `i32` samples.
use super::bits::Bits;
use super::tables::{self, DIRECTION, ANGLE, INVERSE_ANGLE, LEVEL_RUN, MASK4, MASK_LARGE, SCAN_FAMILY};
use super::{Markers, SpiError, Stats};

/// How predicted samples and residuals combine.
#[derive(Clone, Copy)]
pub(super) enum Recon {
    /// Alpha plane: unquantized residual, byte result.
    Alpha,
    /// Colour plane with quantizer zero: unquantized residual, signed 16-bit result.
    ColorLossless,
    /// Colour plane with quantizer `q` (1..=51), clamped to `0..=max`.
    ColorQuantized { q: u8, max: i32 },
}

impl Recon {
    fn q(self) -> u8 {
        match self {
            Recon::ColorQuantized { q, .. } => q,
            _ => 0,
        }
    }
}

pub(super) fn wrap16(v: i32) -> i32 {
    v as i16 as i32
}

/// Byte conversion used by the codec: values outside 0..=255 saturate,
/// except that -32768 maps to 1.
pub(super) fn clip_byte(v: i32) -> i32 {
    let w = wrap16(v);
    if (0..=255).contains(&w) { w } else { ((-w) >> 15) & 255 }
}

struct Partition {
    x: usize,
    y: usize,
    side: usize,
    mode: u8,
    /// 0 = no residual, 1 = only the first coefficient, 7 = full array.
    flag: u8,
    coef: Vec<i32>,
}

/// Decode one plane of an intra block.
///
/// `cell_x` is the block's first 4x4 marker column (4 * block column).
/// `left`/`above` are the completed 33-entry external edge arrays.
/// `out` receives the 16x16 reconstructed samples (row-major).
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_plane(bits: &mut Bits, markers: &mut Markers, cell_x: usize, recon: Recon, left: &[i32; 33],
                           above: &[i32; 33], out: &mut [i32; 256], stats: &mut Stats) -> Result<(), SpiError> {
    let divided = bits.bit()?;
    let sections: &[(usize, usize)] = if divided { &[(0, 0), (8, 0), (0, 8), (8, 8)] } else { &[(0, 0)] };
    let section_side = if divided { 8 } else { 16 };
    let mut decoded = [[false; 4]; 4];
    // Scratch above array kept between partitions (one 4x4 position reuses
    // the tail written by the partition before it).
    let mut scratch_above = [0i32; 33];
    for &(sx, sy) in sections {
        let split = divided && bits.bit()?;
        let side = section_side >> (split as usize);
        let positions: Vec<(usize, usize)> = if split {
            vec![(sx, sy), (sx + side, sy), (sx, sy + side), (sx + side, sy + side)]
        } else {
            vec![(sx, sy)]
        };
        let mut parts: Vec<Partition> = Vec::with_capacity(positions.len());
        for &(x, y) in &positions {
            let cx = (cell_x + x / 4) as isize;
            let cy = (y / 4) as isize;
            let mode = read_mode(bits, markers.at(cx - 1, cy), markers.at(cx, cy - 1))?;
            let cells = side / 4;
            for dy in 0..cells {
                for dx in 0..cells {
                    markers.set(cx as usize + dx, cy as usize + dy, mode);
                }
            }
            stats.prediction_modes[mode as usize] += 1;
            stats.partition_sides[side.trailing_zeros() as usize - 2] += 1;
            parts.push(Partition { x, y, side, mode, flag: 0, coef: Vec::new() });
        }
        let count = parts.len();
        let mask = if side == 4 {
            let p = bits.positive()?;
            if p > 64 {
                return Err(SpiError::Invalid("partition mask code out of range"));
            }
            MASK4[tables::mask4_row(recon.q())][p as usize - 1]
        } else {
            let k = bits.zeros(7)?;
            MASK_LARGE[k as usize]
        };
        for (index, part) in parts.iter_mut().enumerate() {
            if mask >> (count + 1 - index) & 1 == 1 {
                let (coef, flag) = match recon {
                    Recon::ColorQuantized { .. } => quantized_coefficients(bits, part.side)?,
                    _ => lossless_coefficients(bits, part.side, part.mode)?,
                };
                part.coef = coef;
                part.flag = flag;
            }
        }
        for part in &mut parts {
            reconstruct(part, recon, left, above, out, &decoded, &mut scratch_above);
            let cells = part.side / 4;
            for dy in 0..cells {
                for dx in 0..cells {
                    decoded[part.y / 4 + dy][part.x / 4 + dx] = true;
                }
            }
        }
    }
    Ok(())
}

fn read_mode(bits: &mut Bits, left: u8, above: u8) -> Result<u8, SpiError> {
    let norm = |m: u8| if m == 17 { 2 } else { m };
    let (l, a) = (norm(left), norm(above));
    if l > 17 || a > 17 {
        return Err(SpiError::Invalid("prediction marker out of range"));
    }
    if bits.bit()? {
        if l == a || !bits.bit()? { Ok(l) } else { Ok(a) }
    } else if bits.bit()? {
        Ok(if bits.bit()? { 17 } else { 2 })
    } else {
        let index = bits.read(4)? as usize;
        (0u8..18).filter(|&m| m != l && m != 2).nth(index).ok_or(SpiError::Invalid("prediction mode index"))
    }
}

/// Unquantized coefficients (prediction-dependent scan, area-relative escapes).
fn lossless_coefficients(bits: &mut Bits, side: usize, mode: u8) -> Result<(Vec<i32>, u8), SpiError> {
    let area = side * side;
    let family = if side == 16 || mode == 17 { 0 } else { SCAN_FAMILY[mode as usize] };
    let order = tables::scan(side, family);
    let tokens = bits.positive()? as usize;
    if tokens > area {
        return Err(SpiError::Invalid("coefficient count exceeds partition"));
    }
    let mut coef = vec![0i32; area];
    let mut next = 0usize;
    let mut last = 0usize;
    for _ in 0..tokens {
        let code = bits.positive()?;
        let negative = bits.bit()?;
        let (level, run) = if code < 128 {
            let (l, r) = LEVEL_RUN[code as usize - 1];
            (l as u32, r as usize)
        } else {
            let escaped = (code - 128) as usize;
            ((escaped / area) as u32 + 1, escaped % area)
        };
        let index = next + run;
        if index >= area {
            return Err(SpiError::Invalid("coefficient run leaves partition"));
        }
        let value = if negative { (level as i32).wrapping_neg() } else { level as i32 };
        coef[order[index] as usize] = wrap16(value);
        next = index + 1;
        last = index;
    }
    Ok((coef, if tokens == 1 && last == 0 { 1 } else { 7 }))
}

/// Quantized coefficients (zigzag scan, 8-bit run escapes).
fn quantized_coefficients(bits: &mut Bits, side: usize) -> Result<(Vec<i32>, u8), SpiError> {
    let area = side * side;
    let order = tables::scan(side, 0);
    let tokens = bits.positive()? as usize;
    if tokens > area {
        return Err(SpiError::Invalid("coefficient count exceeds partition"));
    }
    let mut coef = vec![0i32; area];
    let mut next = 0usize;
    let mut last = 0usize;
    for _ in 0..tokens {
        let code = bits.positive()?;
        let negative = bits.bit()?;
        let (level, run) = if code < 128 {
            let (l, r) = LEVEL_RUN[code as usize - 1];
            (l as i32, r as usize)
        } else {
            ((((code - 1) >> 8) as i32) - 1, ((code - 1) & 255) as usize)
        };
        let index = next + run;
        if index >= area {
            return Err(SpiError::Invalid("coefficient run leaves partition"));
        }
        let value = if negative { level.wrapping_neg() } else { level };
        coef[order[index] as usize] = wrap16(value);
        next = index + 1;
        last = index;
    }
    Ok((coef, if tokens == 1 && last == 0 { 1 } else { 7 }))
}

/// Edge arrays for one partition (length 2n+1, index 0 = corner).
fn edges(part: &Partition, left: &[i32; 33], above: &[i32; 33], out: &[i32; 256], decoded: &[[bool; 4]; 4],
         scratch_above: &mut [i32; 33]) -> (Vec<i32>, Vec<i32>) {
    let (x, y, n) = (part.x, part.y, part.side);
    let px = |col: usize, row: usize| out[row * 16 + col];
    let mut l = vec![0i32; 2 * n + 1];
    let corner_l;
    let corner_a;
    if x == 0 && y == 0 {
        corner_l = left[0];
        corner_a = above[0];
    } else if x == 0 {
        corner_l = left[y];
        corner_a = left[y];
    } else if y == 0 {
        corner_l = above[x];
        corner_a = above[x];
    } else {
        corner_l = px(x - 1, y - 1);
        corner_a = corner_l;
    }
    l[0] = corner_l;
    for i in 1..=n {
        l[i] = if x == 0 { left[y + i] } else { px(x - 1, y + i - 1) };
    }
    let below_available = x == 0 || (y + n < 16 && decoded[(y + n) / 4][(x - 1) / 4]);
    for i in n + 1..=2 * n {
        l[i] = if x == 0 {
            left[y + i]
        } else if below_available {
            px(x - 1, y + i - 1)
        } else {
            l[n]
        };
    }
    // Above: the 4x4 partition at (4, 4) keeps the extension entries the
    // previous partition left in the scratch array.
    let quirk = n == 4 && x == 4 && y == 4;
    scratch_above[0] = corner_a;
    for i in 1..=n {
        scratch_above[i] = if y == 0 { above[x + i] } else { px(x + i - 1, y - 1) };
    }
    if !quirk {
        let right_available = y == 0 || (x + n < 16 && decoded[(y - 1) / 4][(x + n) / 4]);
        for i in n + 1..=2 * n {
            scratch_above[i] = if y == 0 {
                above[x + i]
            } else if right_available {
                px(x + i - 1, y - 1)
            } else {
                scratch_above[n]
            };
        }
    }
    (l, scratch_above[..=2 * n].to_vec())
}

fn reconstruct(part: &mut Partition, recon: Recon, left: &[i32; 33], above: &[i32; 33], out: &mut [i32; 256],
               decoded: &[[bool; 4]; 4], scratch_above: &mut [i32; 33]) {
    let n = part.side;
    let (l, a) = edges(part, left, above, out, decoded, scratch_above);
    let prediction = predict(part.mode, n, &l, &a);
    let mut residual = std::mem::take(&mut part.coef);
    match recon {
        Recon::Alpha | Recon::ColorLossless => {
            if part.flag != 0 {
                accumulate(&mut residual, n, part.mode);
            }
            for row in 0..n {
                for col in 0..n {
                    let p = prediction[row * n + col];
                    let v = if part.flag == 0 {
                        p
                    } else {
                        let sum = wrap16(p + residual[row * n + col]);
                        if matches!(recon, Recon::Alpha) { clip_byte(sum) } else { sum }
                    };
                    out[(part.y + row) * 16 + part.x + col] = v;
                }
            }
        }
        Recon::ColorQuantized { q, max } => {
            if part.flag != 0 {
                dequantize(&mut residual, n, q, part.flag);
                inverse_transform(&mut residual, n, part.flag);
            }
            for row in 0..n {
                for col in 0..n {
                    let p = prediction[row * n + col];
                    let v = match part.flag {
                        0 => p,
                        1 => wrap16(p + residual[0]).clamp(0, max),
                        _ => wrap16(p + residual[row * n + col]).clamp(0, max),
                    };
                    out[(part.y + row) * 16 + part.x + col] = v;
                }
            }
        }
    }
}

/// Lossless residuals: vertical (mode 0) or horizontal (mode 1) running sums.
fn accumulate(c: &mut [i32], n: usize, mode: u8) {
    match mode {
        0 => {
            for row in 1..n {
                for col in 0..n {
                    c[row * n + col] = wrap16(c[row * n + col] + c[(row - 1) * n + col]);
                }
            }
        }
        1 => {
            for row in 0..n {
                for col in 1..n {
                    c[row * n + col] = wrap16(c[row * n + col] + c[row * n + col - 1]);
                }
            }
        }
        _ => {}
    }
}

fn predict(mode: u8, n: usize, l: &[i32], a: &[i32]) -> Vec<i32> {
    let mut p = vec![0i32; n * n];
    let shift = (2 * n).trailing_zeros();
    if mode == 17 {
        for y in 0..n {
            for x in 0..n {
                let v = n as i32
                    + (n - 1 - x) as i32 * l[y + 1]
                    + (x + 1) as i32 * a[n]
                    + (n - 1 - y) as i32 * a[x + 1]
                    + (y + 1) as i32 * l[n];
                p[y * n + x] = v >> shift;
            }
        }
        return p;
    }
    let (fl, fa);
    let (l_used, a_used): (&[i32], &[i32]) = if tables::smooth(n, mode) {
        fl = smooth_edge(l, l[0], a[1], l[1]);
        fa = smooth_edge(a, l[0], a[1], l[1]);
        (&fl, &fa)
    } else {
        (l, a)
    };
    let direction = DIRECTION[mode as usize] as i32;
    if direction == 0 {
        let sum: i32 = l_used[1..=n].iter().sum::<i32>() + a_used[1..=n].iter().sum::<i32>();
        let dc = (sum + n as i32) >> shift;
        p.iter_mut().for_each(|v| *v = dc);
        if mode == 2 {
            let w: i32 = match n { 4 => 3, 8 => 2, _ => 1 };
            p[0] = (w * (l[1] + a[1]) + (8 - 2 * w) * dc + 4) >> 3;
            for x in 1..n {
                p[x] = (w * a[x + 1] + (8 - w) * dc + 4) >> 3;
            }
            for y in 1..n {
                p[y * n] = (w * l[y + 1] + (8 - w) * dc + 4) >> 3;
            }
        }
        return p;
    }
    let horizontal = direction >= 10;
    let delta = direction - if horizontal { 13 } else { 5 };
    let angle = delta.signum() * ANGLE[delta.unsigned_abs() as usize];
    let (main, secondary) = if horizontal { (l_used, a_used) } else { (a_used, l_used) };
    // Reference with room for n negative indexes.
    let mut reference = vec![0i32; 3 * n + 1];
    reference[n..].copy_from_slice(&main[..=2 * n]);
    if angle < 0 {
        let count = -((n as i32 * angle) >> 5);
        let inverse = INVERSE_ANGLE[delta.unsigned_abs() as usize];
        for i in 1..=count {
            let source = ((128 + i * inverse) >> 8) as usize;
            reference[n - i as usize] = secondary[source.min(2 * n)];
        }
    }
    for pos in 0..n {
        let step = (pos as i32 + 1) * angle;
        let offset = step >> 5;
        let fraction = step - 32 * offset;
        for q in 0..n {
            let index = (q as i32 + offset + 1 + n as i32) as usize;
            let v = if fraction == 0 {
                reference[index]
            } else {
                ((32 - fraction) * reference[index] + fraction * reference[index + 1] + 16) >> 5
            };
            if horizontal { p[q * n + pos] = v } else { p[pos * n + q] = v }
        }
    }
    p
}

/// [1 2 1] smoothing with a corner shared by both edges.
fn smooth_edge(e: &[i32], l0: i32, a1: i32, l1: i32) -> Vec<i32> {
    let len = e.len();
    let mut f = e.to_vec();
    f[0] = (l1 + 2 * l0 + a1 + 2) >> 2;
    for i in 1..len - 1 {
        f[i] = (e[i - 1] + 2 * e[i] + e[i + 1] + 2) >> 2;
    }
    f
}

fn dequantize(c: &mut [i32], n: usize, q: u8, flag: u8) {
    let (quotient, rem) = ((q / 6) as i32, (q % 6) as usize);
    let shift = quotient - match n { 4 => 4, 8 => 6, _ => 3 };
    let count = if flag == 1 { 1 } else { n * n };
    for (i, v) in c.iter_mut().enumerate().take(count) {
        let p = *v * tables::scale(n, rem, i % n, i / n);
        *v = if shift >= 0 { wrap16(p << shift) } else { wrap16((p + (1 << (-shift - 1))) >> -shift) };
    }
}

fn inverse_transform(c: &mut [i32], n: usize, flag: u8) {
    if flag == 1 {
        c[0] = if n == 16 { (c[0] + 65) >> 7 } else { (c[0] + 32) >> 6 };
        return;
    }
    match n {
        4 => transform4(c),
        8 => transform8(c),
        _ => transform16(c),
    }
}

fn transform4(c: &mut [i32]) {
    let mut t = [0i32; 16];
    for col in 0..4 {
        let a = [c[col], c[4 + col], c[8 + col], c[12 + col]];
        let (e0, e1) = (a[0] + a[2], a[0] - a[2]);
        let (o0, o1) = (a[1] + (a[3] >> 1), a[3] - (a[1] >> 1));
        let r = [e0 + o0, e1 - o1, e1 + o1, e0 - o0];
        for k in 0..4 {
            t[k * 4 + col] = wrap16(r[k]);
        }
    }
    for row in 0..4 {
        let a = &t[row * 4..row * 4 + 4];
        let e0 = wrap16(a[0] + a[2]) + 32;
        let e1 = wrap16(a[0] - a[2]) + 32;
        let o0 = wrap16(a[1] + (a[3] >> 1));
        let o1 = wrap16(a[3] - (a[1] >> 1));
        let r = [e0 + o0, e1 - o1, e1 + o1, e0 - o0];
        for k in 0..4 {
            c[k * 4 + row] = wrap16(r[k] >> 6);
        }
    }
}

fn butterfly8(a: [i32; 8]) -> [i32; 8] {
    let e0 = a[0] + a[4];
    let e1 = a[0] - a[4];
    let e2 = a[2] + (a[6] >> 1);
    let e3 = a[6] - (a[2] >> 1);
    let e = [e0 + e2, e1 - e3, e1 + e3, e0 - e2];
    let b0 = a[5] - a[3] - a[7] - (a[7] >> 1);
    let b1 = a[7] - a[3] - (a[3] >> 1) + a[1];
    let b2 = a[7] + a[5] + (a[5] >> 1) - a[1];
    let b3 = a[5] + a[3] + a[1] + (a[1] >> 1);
    let o = [b3 - (b0 >> 2), b2 - (b1 >> 2), b1 + (b2 >> 2), b0 + (b3 >> 2)];
    [e[0] + o[0], e[1] - o[1], e[2] + o[2], e[3] + o[3], e[3] - o[3], e[2] - o[2], e[1] + o[1], e[0] - o[0]]
}

fn transform8(c: &mut [i32]) {
    c[0] = wrap16(c[0] + 32);
    let mut t = [0i32; 64];
    for col in 0..8 {
        let mut a = [0i32; 8];
        for k in 0..8 {
            a[k] = c[k * 8 + col];
        }
        let r = butterfly8(a);
        for k in 0..8 {
            t[k * 8 + col] = wrap16(r[k]);
        }
    }
    for row in 0..8 {
        let mut a = [0i32; 8];
        a.copy_from_slice(&t[row * 8..row * 8 + 8]);
        let r = butterfly8(a);
        for k in 0..8 {
            c[k * 8 + row] = wrap16(r[k] >> 6);
        }
    }
}

fn transform16(c: &mut [i32]) {
    let m = tables::basis16();
    let mut t = [[0i32; 16]; 16];
    for col in 0..16 {
        for sample in 0..16 {
            let sum: i64 = (0..16).map(|k| m[k][sample] as i64 * c[k * 16 + col] as i64).sum();
            t[col][sample] = wrap16(((sum + 64) >> 7) as i32);
        }
    }
    for row in 0..16 {
        for col in 0..16 {
            let sum: i64 = (0..16).map(|k| m[k][col] as i64 * t[k][row] as i64).sum();
            c[row * 16 + col] = wrap16(((sum + 2048) >> 12) as i32);
        }
    }
}
