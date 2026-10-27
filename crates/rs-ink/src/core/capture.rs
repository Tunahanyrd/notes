//! ink_capture: capture quantization and basic-pen bounds.
use super::binary::{decode_point_channels, RawSamples, Samples, MAX_POINTS};
use super::num::{f32r, py_max, py_min, Val};
use super::{ink, R};

pub const MAX_CAPTURE_COORDINATE: f64 = 100000.0;
pub const BASIC_PENS: [&str; 2] = [
    "com.samsung.android.sdk.pen.pen.preload.FountainPen",
    "com.samsung.android.sdk.pen.pen.preload.InkPen2",
];

#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedCapture {
    pub samples: Samples,
    pub channels: Vec<u8>,
    pub stylus: bool,
    pub max_coordinate_error: f64,
    pub max_pressure_error: f64,
}

fn number(v: &Val) -> R<f64> {
    if !v.is_num() {
        return ink("invalid capture number");
    }
    v.f32v()
}

/// ink_capture.normalize_capture
pub fn normalize_capture(s: &RawSamples, stylus: bool) -> R<NormalizedCapture> {
    let n = s.points.len();
    if !(1..=MAX_POINTS).contains(&n) || s.pressures.len() != n || s.timestamps.len() != n {
        return ink("capture channel count bound");
    }
    let want = if stylus { (n, n) } else { (0, 0) };
    if (s.tilts.len(), s.orientations.len()) != want {
        return ink("capture stylus channels differ");
    }
    let mut points: Vec<(f64, f64)> = Vec::with_capacity(n);
    for p in &s.points {
        let p = match p {
            Some(p) => p,
            None => return ink("invalid captured point"),
        };
        let x = number(&p[0])?;
        let y = number(&p[1])?;
        if x.abs() > MAX_CAPTURE_COORDINATE || y.abs() > MAX_CAPTURE_COORDINATE {
            return ink("capture coordinate bound");
        }
        points.push((x, y));
    }
    let pressures = s.pressures.iter().map(number).collect::<R<Vec<f64>>>()?;
    if pressures.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return ink("capture pressure must be 0..1");
    }
    let mut times: Vec<i64> = Vec::with_capacity(n);
    for t in &s.timestamps {
        match t {
            Val::Int(x) if (0..(1i128 << 31)).contains(x) => times.push(*x as i64),
            _ => return ink("invalid relative millisecond timestamp"),
        }
    }
    if times[0] != 0 {
        return ink("capture timestamp must begin at zero");
    }
    if times.windows(2).any(|w| !(0..=65535).contains(&(w[1] - w[0]))) {
        return ink("capture timestamp gap bound");
    }
    let tilts = s.tilts.iter().map(number).collect::<R<Vec<f64>>>()?;
    let orientations = s.orientations.iter().map(number).collect::<R<Vec<f64>>>()?;
    let mut out: Vec<u8> = Vec::new();
    out.extend(points[0].0.to_le_bytes());
    out.extend(points[0].1.to_le_bytes());
    let mut previous = points[0];
    for point in &points[1..] {
        let mut restored = [0f64; 2];
        for (i, (current, prior)) in [(point.0, previous.0), (point.1, previous.1)].into_iter().enumerate() {
            let bias = if current > prior { 1.0 / 64.0 } else { -1.0 / 64.0 };
            let biased = f32r(f32r(current - prior)? + bias)?;
            let magnitude = (biased.abs() * 32.0).trunc();
            if magnitude > 32767.0 {
                return ink("capture coordinate delta bound");
            }
            let m = magnitude as u16;
            out.extend((m | if biased < 0.0 { 0x8000 } else { 0 }).to_le_bytes());
            restored[i] = f32r(prior + if biased < 0.0 { -(m as f64) / 32.0 } else { m as f64 / 32.0 })?;
        }
        previous = (restored[0], restored[1]);
    }
    let floats = |values: &[f64], out: &mut Vec<u8>| -> R<()> {
        out.extend((values[0] as f32).to_le_bytes());
        for w in values.windows(2) {
            let delta = f32r(w[1] - w[0])?;
            let magnitude = (delta.abs() * 4096.0).trunc();
            if magnitude > 32767.0 {
                return ink("capture float delta bound");
            }
            out.extend((magnitude as u16 | if delta < 0.0 { 0x8000 } else { 0 }).to_le_bytes());
        }
        Ok(())
    };
    floats(&pressures, &mut out)?;
    out.extend((times[0] as i32).to_le_bytes());
    for w in times.windows(2) {
        out.extend(((w[1] - w[0]) as u16).to_le_bytes());
    }
    if stylus {
        floats(&tilts, &mut out)?;
        floats(&orientations, &mut out)?;
    }
    let normalized = decode_point_channels(&out, n, true, stylus)?;
    if normalized.pressures.iter().any(|p| !(0.0..=1.0).contains(p)) {
        return ink("quantized capture pressure leaves 0..1");
    }
    let mut xy_error = f64::NEG_INFINITY;
    for (p, q) in points.iter().zip(&normalized.points) {
        for v in [(p.0 - q.0).abs(), (p.1 - q.1).abs()] {
            xy_error = py_max(xy_error, v);
        }
    }
    let mut pressure_error = f64::NEG_INFINITY;
    for (a, b) in pressures.iter().zip(&normalized.pressures) {
        pressure_error = py_max(pressure_error, (a - b).abs());
    }
    Ok(NormalizedCapture { samples: normalized, channels: out, stylus, max_coordinate_error: xy_error,
                           max_pressure_error: pressure_error })
}

/// ink_capture.basic_pen_drawn_bounds
pub fn basic_pen_drawn_bounds(points: Option<&[Option<[Val; 2]>]>, pen_name: Option<&str>, pen_size: Val,
                              rotation: Val) -> R<[f64; 4]> {
    if !pen_name.is_some_and(|p| BASIC_PENS.contains(&p)) {
        return ink("unsupported brush bounds profile");
    }
    if !rotation.is_num() || !rotation.is_zero() {
        return ink("rotated stroke bounds unsupported");
    }
    let size_ok = match pen_size {
        Val::Int(i) => 0 < i && i <= 256,
        Val::Float(f) => 0.0 < f && f <= 256.0 && f.is_finite(),
        _ => false,
    };
    if !size_ok {
        return ink("brush size outside supported bound");
    }
    let points = match points {
        Some(p) if (1..=MAX_POINTS).contains(&p.len()) => p,
        _ => return ink("invalid bounds samples"),
    };
    let mut xs = Vec::with_capacity(points.len());
    let mut ys = Vec::with_capacity(points.len());
    for p in points {
        let p = match p {
            Some(p) => p,
            None => return ink("invalid bounds point"),
        };
        for v in p {
            if !v.is_num() || v.abs_gt(MAX_CAPTURE_COORDINATE) || !v.is_finite()? {
                return ink("invalid bounds coordinate");
            }
        }
        xs.push(p[0].f32v()?);
        ys.push(p[1].f32v()?);
    }
    let extent = f32r(f32r(pen_size.f32v()? * 0.5)? + 4.0)?;
    let fold = |v: &[f64], f: fn(f64, f64) -> f64| v[1..].iter().fold(v[0], |a, b| f(a, *b));
    Ok([
        f32r(fold(&xs, py_min) - extent)?,
        f32r(fold(&ys, py_min) - extent)?,
        f32r(fold(&xs, py_max) + extent)?,
        f32r(fold(&ys, py_max) + extent)?,
    ])
}
