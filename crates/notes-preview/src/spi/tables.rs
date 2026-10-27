//! Constant tables of the SPI canvas-cache format (see docs/CANVAS_CACHE.md).
//!
//! Scan orders and the scale/transform tables are generated from their
//! defining rules; the two code tables (run/level pairs and partition masks)
//! are format constants listed as data.

/// Scan family per prediction mode for unquantized coefficients
/// (0 = zigzag, 1 = rows, 2 = columns).
pub(super) const SCAN_FAMILY: [u8; 18] = [1, 2, 0, 0, 1, 1, 0, 2, 2, 0, 0, 1, 1, 0, 0, 2, 2, 0];

/// `(level, zero run)` for coefficient codes 1..=127.
pub(super) const LEVEL_RUN: [(u8, u8); 127] = [
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

/// Coded-partition masks for 4x4 partitions, one row per quantizer class.
pub(super) const MASK4: [[u8; 64]; 5] = [
    [63, 60, 62, 61, 28, 52, 44, 56, 31, 47, 55, 59, 12, 48, 30, 54, 46, 58, 20, 29, 53, 45, 40, 57, 51, 15, 0, 16, 4,
     14, 50, 32, 8, 13, 49, 36, 24, 22, 43, 23, 42, 21, 41, 38, 34, 26, 18, 6, 39, 10, 2, 33, 17, 27, 5, 37, 35, 1,
     25, 9, 19, 7, 11, 3],
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

/// Mask row for a 4x4 partition at quantizer `q` (0..=51).
pub(super) fn mask4_row(q: u8) -> usize {
    match q {
        22..=26 => 1,
        27..=31 => 2,
        32..=36 => 3,
        37..=50 => 4,
        _ => 0,
    }
}

/// Coded-partition masks for 8x8/16x16 partitions, indexed by a unary code.
pub(super) const MASK_LARGE: [u8; 8] = [4, 0, 6, 7, 5, 2, 1, 3];

/// Edge smoothing before prediction, per partition side (4, 8, 16) and mode 0..=16.
pub(super) fn smooth(side: usize, mode: u8) -> bool {
    match side {
        4 => matches!(mode, 3 | 6 | 9),
        8 => (3..=9).contains(&mode),
        _ => mode >= 3,
    }
}

/// Direction code for prediction modes 0..=16 (0 = DC).
pub(super) const DIRECTION: [u8; 17] = [5, 13, 0, 1, 3, 7, 9, 11, 15, 17, 2, 4, 6, 8, 10, 12, 14];
pub(super) const ANGLE: [i32; 5] = [0, 5, 13, 21, 32];
pub(super) const INVERSE_ANGLE: [i32; 5] = [0, 1638, 630, 390, 256];

/// Scan order (coefficient index for each scan position).
pub(super) fn scan(side: usize, family: u8) -> Vec<u16> {
    let mut order = Vec::with_capacity(side * side);
    match family {
        1 => order.extend((0..side * side).map(|i| i as u16)),
        2 => {
            for col in 0..side {
                for row in 0..side {
                    order.push((row * side + col) as u16);
                }
            }
        }
        _ => {
            for d in 0..(2 * side - 1) {
                let low = d.saturating_sub(side - 1);
                let high = d.min(side - 1);
                let rows: Vec<usize> = if d % 2 == 0 { (low..=high).collect() } else { (low..=high).rev().collect() };
                for row in rows {
                    order.push((row * side + (d - row)) as u16);
                }
            }
        }
    }
    order
}

/// Dequantization scale for 4x4 coefficients by `(Q % 6, class)`.
const SCALE4: [[i32; 3]; 6] = [
    [160, 208, 256], [176, 224, 288], [208, 256, 320], [224, 288, 368], [256, 320, 400], [288, 368, 464],
];
/// Dequantization scale for 8x8 coefficients by `(Q % 6, class)`.
const SCALE8: [[i32; 6]; 6] = [
    [320, 304, 400, 288, 384, 512],
    [352, 336, 448, 304, 416, 560],
    [416, 384, 528, 368, 496, 672],
    [448, 416, 560, 400, 528, 720],
    [512, 480, 640, 448, 608, 816],
    [576, 544, 736, 512, 688, 928],
];
const CLASS8: [[usize; 4]; 4] = [[0, 1, 2, 1], [1, 3, 4, 3], [2, 4, 5, 4], [1, 3, 4, 3]];
const SCALE16: [i32; 6] = [40, 45, 51, 57, 64, 72];

pub(super) fn scale(side: usize, rem: usize, x: usize, y: usize) -> i32 {
    match side {
        4 => SCALE4[rem][(y % 2) + (x % 2)],
        8 => SCALE8[rem][CLASS8[y % 4][x % 4]],
        _ => SCALE16[rem],
    }
}

/// First half of the 16-point inverse transform basis (rows = frequency).
const BASIS16_HALF: [[i32; 8]; 16] = [
    [64, 64, 64, 64, 64, 64, 64, 64],
    [90, 87, 80, 70, 57, 43, 25, 9],
    [89, 75, 50, 18, -18, -50, -75, -89],
    [87, 57, 9, -43, -80, -90, -70, -25],
    [83, 36, -36, -83, -83, -36, 36, 83],
    [80, 9, -70, -87, -25, 57, 90, 43],
    [75, -18, -89, -50, 50, 89, 18, -75],
    [70, -43, -87, 9, 90, 25, -80, -57],
    [64, -64, -64, 64, 64, -64, -64, 64],
    [57, -80, -25, 90, -9, -87, 43, 70],
    [50, -89, 18, 75, -75, -18, 89, -50],
    [43, -90, 57, 25, -87, 70, 9, -80],
    [36, -83, 83, -36, -36, 83, -83, 36],
    [25, -70, 90, -80, 43, 9, -57, 87],
    [18, -50, 75, -89, 89, -75, 50, -18],
    [9, -25, 43, -57, 70, -80, 87, -90],
];

/// Full 16x16 basis: even rows are symmetric, odd rows antisymmetric.
pub(super) fn basis16() -> [[i32; 16]; 16] {
    let mut m = [[0i32; 16]; 16];
    for (r, half) in BASIS16_HALF.iter().enumerate() {
        for c in 0..8 {
            m[r][c] = half[c];
            m[r][15 - c] = if r % 2 == 0 { half[c] } else { -half[c] };
        }
    }
    m
}
