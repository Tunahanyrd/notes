//! Spen stroke binary codec (ink.py / ink_editor.py byte layer).
use super::num::{f32r, Val};
use super::{ink, InkErr, R};

pub const MAX_STROKE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CANVAS_DIMENSION: u64 = 32768;
pub const MAX_CANVAS_PIXELS: u64 = 64 * 1024 * 1024;
pub const MAX_POINTS: usize = 32768;
pub const WRAPPER: [u8; 12] = [0xff, 0xff, 0xff, 0xff, 4, 0, 0, 0, 0xa0, 0x0f, 0, 0];

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub raw: Vec<u8>,
    pub kind: i64,
    pub properties: Vec<u8>,
    pub fields: Vec<u8>,
    pub fixed_start: i64,
    pub flexible_start: i64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Samples {
    pub points: Vec<(f64, f64)>,
    pub pressures: Vec<f64>,
    pub timestamps: Vec<i64>,
    pub tilts: Vec<f64>,
    pub orientations: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub raw: Vec<u8>,
    pub frames: Vec<Frame>,
    pub samples: Samples,
    pub compressed: bool,
    pub stylus: bool,
    pub tool_type: u16,
}

/// Samples as received from Python, before validation.
#[derive(Clone, Debug, Default)]
pub struct RawSamples {
    /// None: element is not a 2-item tuple/list.
    pub points: Vec<Option<[Val; 2]>>,
    pub pressures: Vec<Val>,
    pub timestamps: Vec<Val>,
    pub tilts: Vec<Val>,
    pub orientations: Vec<Val>,
}

impl RawSamples {
    /// Typed capture input: page-unit XY, pressure 0..1, relative integer ms.
    pub fn from_parts(points: &[(f64, f64)], pressures: &[f64], timestamps: &[i64]) -> Self {
        RawSamples {
            points: points.iter().map(|p| Some([Val::Float(p.0), Val::Float(p.1)])).collect(),
            pressures: pressures.iter().map(|v| Val::Float(*v)).collect(),
            timestamps: timestamps.iter().map(|v| Val::Int(*v as i128)).collect(),
            tilts: vec![],
            orientations: vec![],
        }
    }
    pub fn from_samples(s: &Samples) -> Self {
        RawSamples {
            points: s.points.iter().map(|p| Some([Val::Float(p.0), Val::Float(p.1)])).collect(),
            pressures: s.pressures.iter().map(|v| Val::Float(*v)).collect(),
            timestamps: s.timestamps.iter().map(|v| Val::Int(*v as i128)).collect(),
            tilts: s.tilts.iter().map(|v| Val::Float(*v)).collect(),
            orientations: s.orientations.iter().map(|v| Val::Float(*v)).collect(),
        }
    }
}

/// Python slice `data[start:end]` with negative/clamped indices.
pub fn py_slice(data: &[u8], start: i64, end: Option<i64>) -> &[u8] {
    let n = data.len() as i64;
    let norm = |i: i64| if i < 0 { (i + n).max(0) } else { i.min(n) };
    let s = norm(start);
    let e = end.map(norm).unwrap_or(n);
    if s >= e { &[] } else { &data[s as usize..e as usize] }
}

fn need(data: &[u8], off: i64, size: usize) -> R<usize> {
    let n = data.len() as i64;
    let o = if off < 0 { off + n } else { off };
    if o < 0 || o + size as i64 > n {
        return Err(InkErr::Struct(format!(
            "unpack_from requires a buffer of at least {} bytes for unpacking {} bytes at offset {} (actual buffer size is {})",
            off + size as i64, size, off, n)));
    }
    Ok(o as usize)
}

pub fn u16_at(d: &[u8], off: i64) -> R<u16> {
    let o = need(d, off, 2)?;
    Ok(u16::from_le_bytes([d[o], d[o + 1]]))
}
pub fn u32_at(d: &[u8], off: i64) -> R<u32> {
    let o = need(d, off, 4)?;
    Ok(u32::from_le_bytes(d[o..o + 4].try_into().unwrap()))
}
pub fn i32_at(d: &[u8], off: i64) -> R<i32> {
    Ok(u32_at(d, off)? as i32)
}
pub fn i64_at(d: &[u8], off: i64) -> R<i64> {
    let o = need(d, off, 8)?;
    Ok(i64::from_le_bytes(d[o..o + 8].try_into().unwrap()))
}
pub fn f32_at(d: &[u8], off: i64) -> R<f64> {
    Ok(f32::from_bits(u32_at(d, off)?) as f64)
}
pub fn f64_at(d: &[u8], off: i64) -> R<f64> {
    Ok(f64::from_bits(i64_at(d, off)? as u64))
}
pub fn put(d: &mut [u8], off: usize, bytes: &[u8]) -> R<()> {
    if off + bytes.len() > d.len() {
        return Err(InkErr::Struct(format!("pack_into requires a buffer of at least {} bytes", off + bytes.len())));
    }
    d[off..off + bytes.len()].copy_from_slice(bytes);
    Ok(())
}

/// int.from_bytes(b, 'little') truncated to 64 bits, plus whether higher bits exist.
pub fn mask64(b: &[u8]) -> (u64, bool) {
    let mut v = 0u64;
    let mut high = false;
    for (i, x) in b.iter().enumerate() {
        if i < 8 {
            v |= (*x as u64) << (8 * i);
        } else if *x != 0 {
            high = true;
        }
    }
    (v, high)
}

/// ink.unwrap_stroke_resource
pub fn unwrap_stroke_resource(data: &[u8]) -> R<&[u8]> {
    if !(13..=MAX_STROKE_BYTES + 12).contains(&data.len()) {
        return ink("stroke resource exceeds bounds or is empty");
    }
    if u32_at(data, 0)? != 0xffffffff || u32_at(data, 4)? != 4 || u32_at(data, 8)? != 4000 {
        return ink("unsupported stroke resource wrapper");
    }
    Ok(&data[12..])
}

/// Whether unwrap_stroke_resource accepts a body of `len` bytes starting with `head`.
pub fn wrapper_ok(head: &[u8], len: usize) -> bool {
    (13..=MAX_STROKE_BYTES + 12).contains(&len)
        && head.len() >= 12
        && head[..4] == [0xff; 4]
        && head[4..8] == [4, 0, 0, 0]
        && head[8..12] == [0xa0, 0x0f, 0, 0]
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpiHeader {
    pub width: u16,
    pub height: u16,
    pub color_index: u8,
    pub row_group_size: u16,
    pub reference_cache_capacity: u8,
    pub flags: u8,
    pub encoded_data_bytes: u32,
}

/// ink.inspect_spi
pub fn inspect_spi(data: &[u8]) -> R<SpiHeader> {
    if !(29..=8 * 1024 * 1024).contains(&data.len()) {
        return ink("SPI input exceeds bounds or is truncated");
    }
    if u32_at(data, 0)? != 20 {
        return ink("unsupported SPI header size");
    }
    let h = &data[4..24];
    let be16 = |o: usize| u16::from_be_bytes([h[o], h[o + 1]]);
    if h[..2] != [0xaa, 0x01] || u32::from_be_bytes(h[2..6].try_into().unwrap()) != 20 {
        return ink("unsupported SPI header packet");
    }
    let payload = u32_at(data, 24)?;
    if payload == 0 || 28 + payload as u64 != data.len() as u64 {
        return ink("SPI data length mismatch or trailing data");
    }
    let (cw, ch, w, hh) = (be16(7) as u64, be16(9) as u64, be16(11) as u64, be16(13) as u64);
    if !(0 < w && w <= MAX_CANVAS_DIMENSION) || !(0 < hh && hh <= MAX_CANVAS_DIMENSION)
        || cw.max(ch) > MAX_CANVAS_DIMENSION || w * hh > MAX_CANVAS_PIXELS
    {
        return ink("SPI image dimensions exceed limit");
    }
    if cw != 0 && ch != 0 && (cw < w || ch < hh) {
        return ink("SPI capacity smaller than image");
    }
    let color = h[15];
    let row_group = be16(16) as u64;
    if !matches!(color, 2 | 4 | 5) {
        return ink("unsupported SPI color index");
    }
    if !(1 <= row_group && row_group <= (hh + 15) / 16) {
        return ink("unsupported SPI row grouping");
    }
    if h[19] & 0x0f != 0 {
        return ink("unsupported SPI reserved flags");
    }
    Ok(SpiHeader { width: w as u16, height: hh as u16, color_index: color, row_group_size: row_group as u16,
                   reference_cache_capacity: h[18], flags: h[19], encoded_data_bytes: payload })
}

/// ink_editor._frames
pub fn parse_frames(data: &[u8]) -> R<Vec<Frame>> {
    let mut result: Vec<Frame> = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        if result.len() >= 16 || data.len() - offset < 12 {
            return ink("invalid stroke frame chain");
        }
        let length = u32_at(data, offset as i64)? as usize;
        let kind = i16::from_le_bytes([data[offset + 4], data[offset + 5]]) as i64;
        let flex = u32_at(data, offset as i64 + 6)? as usize;
        if !(12 <= length && length <= data.len() - offset) {
            return ink("invalid stroke frame length");
        }
        let raw = &data[offset..offset + length];
        let mut p = 10usize;
        let mut masks: Vec<Vec<u8>> = Vec::with_capacity(2);
        for _ in 0..2 {
            if p >= length {
                return ink("truncated frame mask");
            }
            let n = raw[p] as usize;
            p += 1;
            if n > 8 || p + n > length {
                return ink("unsupported frame mask");
            }
            masks.push(raw[p..p + n].to_vec());
            p += n;
        }
        if !(p <= flex && flex <= length) {
            return ink("invalid flexible-data boundary");
        }
        let fields = masks.pop().unwrap();
        let properties = masks.pop().unwrap();
        result.push(Frame { raw: raw.to_vec(), kind, properties, fields, fixed_start: p as i64, flexible_start: flex as i64 });
        offset += length;
    }
    if result.len() < 2 || result[0].kind != 0 || result[1].kind != 1 {
        return ink("expected base and stroke frames");
    }
    if result[2..].iter().any(|f| f.kind == 0 || f.kind == 1) {
        return ink("duplicate base/stroke frame");
    }
    Ok(result)
}

fn delta(word: u16, scale: f64) -> f64 {
    let m = (word & 0x7fff) as f64 / scale;
    if word & 0x8000 != 0 { -m } else { m }
}

pub fn channel_len(count: usize, compressed: bool, stylus: bool) -> usize {
    if count == 0 {
        0
    } else if compressed {
        16 + 4 * (count - 1) + (2 + 2 * stylus as usize) * (4 + 2 * (count - 1))
    } else {
        count * (24 + 8 * stylus as usize)
    }
}

/// ink_editor.decode_point_channels (count already validated <= MAX_POINTS)
pub fn decode_point_channels(data: &[u8], count: usize, compressed: bool, stylus: bool) -> R<Samples> {
    if data.len() != channel_len(count, compressed, stylus) {
        return ink("stroke channel length mismatch");
    }
    let mut p = 0i64;
    let mut points = Vec::with_capacity(count);
    if count > 0 && compressed {
        let mut x = f32r(f64_at(data, 0)?)?;
        let mut y = f32r(f64_at(data, 8)?)?;
        p = 16;
        points.push((x, y));
        for _ in 1..count {
            let dx = u16_at(data, p)?;
            let dy = u16_at(data, p + 2)?;
            p += 4;
            x = f32r(x + delta(dx, 32.0))?;
            y = f32r(y + delta(dy, 32.0))?;
            points.push((x, y));
        }
    } else {
        for _ in 0..count {
            let x = f64_at(data, p)?;
            let y = f64_at(data, p + 8)?;
            p += 16;
            if !x.is_finite() || !y.is_finite() {
                return ink("non-finite stroke coordinate");
            }
            points.push((x, y));
        }
    }
    let floats = |p: &mut i64| -> R<Vec<f64>> {
        let mut values: Vec<f64> = Vec::with_capacity(count);
        for i in 0..count {
            let value = if compressed && i > 0 {
                let w = u16_at(data, *p)?;
                *p += 2;
                f32r(values[i - 1] + delta(w, 4096.0))?
            } else {
                let v = f32_at(data, *p)?;
                *p += 4;
                v
            };
            if !value.is_finite() {
                return ink("non-finite stroke channel");
            }
            values.push(value);
        }
        Ok(values)
    };
    let pressures = floats(&mut p)?;
    let mut timestamps: Vec<i64> = Vec::with_capacity(count);
    for i in 0..count {
        let t = if compressed && i > 0 {
            let w = u16_at(data, p)?;
            p += 2;
            timestamps[i - 1] + w as i64
        } else {
            let v = i32_at(data, p)?;
            p += 4;
            v as i64
        };
        if !(-(1i64 << 31) <= t && t < (1i64 << 31)) {
            return ink("stroke timestamp overflow");
        }
        timestamps.push(t);
    }
    let (tilts, orientations) = if stylus { (floats(&mut p)?, floats(&mut p)?) } else { (vec![], vec![]) };
    Ok(Samples { points, pressures, timestamps, tilts, orientations })
}

const LOSSY: &str = "delta requires lossy quantization or exceeds native range";

fn word(value: f64, scale: f64) -> R<u16> {
    let m = value.abs() * scale;
    if !m.is_finite() || m != m.trunc() || m > 32767.0 {
        return ink(LOSSY);
    }
    Ok(m as u16 | if value < 0.0 { 0x8000 } else { 0 })
}

/// Exact Python difference a - b for two numbers (ints stay exact).
fn sub(a: &Val, b: &Val) -> R<f64> {
    match (a, b) {
        (Val::Int(x), Val::Int(y)) => Ok((x - y) as f64),
        _ => Ok(a.to_f64()? - b.to_f64()?),
    }
}

/// ink_editor.encode_point_channels without the final round-trip comparison
/// (callers compare with their own equality semantics).
pub fn encode_point_channels_raw(s: &RawSamples, compressed: bool, stylus: bool) -> R<Vec<u8>> {
    let count = s.points.len();
    if count > MAX_POINTS || s.pressures.len() != count || s.timestamps.len() != count {
        return ink("stroke channel counts differ");
    }
    let want = if stylus { (count, count) } else { (0, 0) };
    if (s.tilts.len(), s.orientations.len()) != want {
        return ink("stylus channel counts differ");
    }
    let mut out: Vec<u8> = Vec::new();
    let mut points: Vec<[Val; 2]> = Vec::with_capacity(count);
    for point in &s.points {
        let pt = match point {
            Some(p) => *p,
            None => return ink("invalid point"),
        };
        for v in &pt {
            if !v.is_num() || !v.is_finite()? {
                return ink("invalid coordinate");
            }
        }
        if compressed && (!pt[0].eq_f64(pt[0].f32v()?) || !pt[1].eq_f64(pt[1].f32v()?)) {
            return ink("compressed coordinate loses float32 precision");
        }
        points.push(pt);
    }
    if count > 0 && compressed {
        out.extend(points[0][0].to_f64()?.to_le_bytes());
        out.extend(points[0][1].to_f64()?.to_le_bytes());
        for w in points.windows(2) {
            let a = word(sub(&w[1][0], &w[0][0])?, 32.0)?;
            let b = word(sub(&w[1][1], &w[0][1])?, 32.0)?;
            out.extend(a.to_le_bytes());
            out.extend(b.to_le_bytes());
        }
    } else {
        for p in &points {
            out.extend(p[0].to_f64()?.to_le_bytes());
            out.extend(p[1].to_f64()?.to_le_bytes());
        }
    }
    let floats = |values: &[Val], out: &mut Vec<u8>| -> R<()> {
        for (i, v) in values.iter().enumerate() {
            if !v.is_num() || !v.eq_f64(v.f32v()?) {
                return ink("float channel loses float32 precision");
            }
            if compressed && i > 0 {
                out.extend(word(sub(v, &values[i - 1])?, 4096.0)?.to_le_bytes());
            } else {
                out.extend((v.to_f64()? as f32).to_le_bytes());
            }
        }
        Ok(())
    };
    floats(&s.pressures, &mut out)?;
    let mut prev: i128 = 0;
    for (i, t) in s.timestamps.iter().enumerate() {
        let tv = match t {
            Val::Int(x) if (-(1i128 << 31)..(1i128 << 31)).contains(x) => *x,
            _ => return ink("invalid timestamp"),
        };
        if compressed && i > 0 {
            let d = tv - prev;
            if !(0..=65535).contains(&d) {
                return ink("timestamp delta outside native range");
            }
            out.extend((d as u16).to_le_bytes());
        } else {
            out.extend((tv as i32).to_le_bytes());
        }
        prev = tv;
    }
    if stylus {
        floats(&s.tilts, &mut out)?;
        floats(&s.orientations, &mut out)?;
    }
    Ok(out)
}

/// ink_editor.encode_point_channels for already-typed samples.
pub fn encode_point_channels(s: &Samples, compressed: bool, stylus: bool) -> R<Vec<u8>> {
    let encoded = encode_point_channels_raw(&RawSamples::from_samples(s), compressed, stylus)?;
    if decode_point_channels(&encoded, s.points.len(), compressed, stylus)? != *s {
        return ink("native float accumulation changes requested samples");
    }
    Ok(encoded)
}

/// ink_editor.decode_stroke_resource
pub fn decode_stroke_resource(raw: &[u8]) -> R<Record> {
    if !(13..=MAX_STROKE_BYTES + 12).contains(&raw.len()) {
        return ink("stroke resource exceeds bounds");
    }
    let (marker, length, version) = (u32_at(raw, 0)?, u32_at(raw, 4)?, u32_at(raw, 8)?);
    if marker != 0xffffffff || length != 4 || !(version == 0 || version == 4000) {
        return ink("unsupported editor stroke wrapper");
    }
    let frames = parse_frames(&raw[12..])?;
    let f0 = &frames[0];
    if (f0.raw.len() as i64) - f0.fixed_start < 4 || u32_at(&f0.raw, f0.fixed_start)? != 4000 {
        return ink("unsupported native base format");
    }
    let frame = &frames[1];
    let properties = mask64(&frame.properties).0;
    let compressed = properties & 1 != 0;
    let stylus = properties & 4 != 0;
    let fixed = &frame.raw[frame.fixed_start as usize..frame.flexible_start as usize];
    if fixed.len() < 4 {
        return ink("missing stroke point count or tool type");
    }
    let count = u16::from_le_bytes([fixed[0], fixed[1]]) as usize;
    let tool = u16::from_le_bytes([fixed[fixed.len() - 2], fixed[fixed.len() - 1]]);
    if count > MAX_POINTS {
        return ink("invalid channel profile");
    }
    let samples = decode_point_channels(&fixed[2..fixed.len() - 2], count, compressed, stylus)?;
    Ok(Record { raw: raw.to_vec(), frames, samples, compressed, stylus, tool_type: tool })
}

/// Rebuild `record` with new encoded channels (replace_stroke_channels_offline body).
pub fn rebuild_with_channels(record: &Record, count: usize, channels: &[u8]) -> R<Vec<u8>> {
    let frame = &record.frames[1];
    let fs = frame.fixed_start as usize;
    let mut fixed = (count as u16).to_le_bytes().to_vec();
    fixed.extend_from_slice(channels);
    fixed.extend(record.tool_type.to_le_bytes());
    let flex = fs + fixed.len();
    let mut rebuilt = frame.raw[..fs].to_vec();
    rebuilt.extend_from_slice(&fixed);
    rebuilt.extend_from_slice(py_slice(&frame.raw, frame.flexible_start, None));
    let n = rebuilt.len() as u32;
    put(&mut rebuilt, 0, &n.to_le_bytes())?;
    put(&mut rebuilt, 6, &(flex as u32).to_le_bytes())?;
    let mut result = record.raw[..12].to_vec();
    result.extend_from_slice(&record.frames[0].raw);
    result.extend_from_slice(&rebuilt);
    for f in &record.frames[2..] {
        result.extend_from_slice(&f.raw);
    }
    if result.len() > MAX_STROKE_BYTES + 12 {
        return ink("stroke resource exceeds bounds");
    }
    Ok(result)
}

/// ink_editor.replace_stroke_channels_offline for typed samples.
pub fn replace_stroke_channels_offline(record: &Record, samples: &Samples) -> R<Vec<u8>> {
    let checked = decode_stroke_resource(&record.raw)?;
    if checked != *record {
        return ink("stroke record no longer matches original bytes");
    }
    if *samples == record.samples {
        return Ok(record.raw.clone());
    }
    let channels = encode_point_channels(samples, record.compressed, record.stylus)?;
    let result = rebuild_with_channels(record, samples.points.len(), &channels)?;
    if decode_stroke_resource(&result)?.samples != *samples {
        return ink("stroke reconstruction mismatch");
    }
    Ok(result)
}

// ---- style fields -------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fmt {
    I32,
    U32,
    F32,
    U8,
    U16,
}

impl Fmt {
    pub fn size(self) -> usize {
        match self {
            Fmt::I32 | Fmt::U32 | Fmt::F32 => 4,
            Fmt::U8 => 1,
            Fmt::U16 => 2,
        }
    }
}

pub fn style_field(bit: u32) -> Option<(&'static str, Fmt)> {
    Some(match bit {
        0 => ("legacyPenNameId", Fmt::I32),
        1 => ("advancedSettingId", Fmt::I32),
        2 => ("colorARGB", Fmt::U32),
        3 => ("penSize", Fmt::F32),
        4 => ("field4", Fmt::U8),
        7 => ("penNameId", Fmt::I32),
        8 => ("fixedWidth", Fmt::F32),
        9 => ("sizeLevel", Fmt::I32),
        10 => ("particleDensity", Fmt::I32),
        11 => ("renderingLevel", Fmt::I32),
        12 => ("originalWidth", Fmt::I32),
        13 => ("initialTolerance", Fmt::F32),
        14 => ("lineType", Fmt::U16),
        15 => ("dashOffset", Fmt::F32),
        16 => ("strokeType", Fmt::U16),
        17 => ("penRepeatDistance", Fmt::F32),
        18 => ("particleSize", Fmt::F32),
        19 => ("patternIndex", Fmt::I32),
        20 => ("patternScale", Fmt::F32),
        21 => ("particleLevel", Fmt::I32),
        22 => ("rainbowDistance", Fmt::I32),
        23 => ("rainbowOffset", Fmt::F32),
        25 => ("colorType", Fmt::U16),
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub enum StyleVal {
    I(i64),
    F(f64),
    G(Vec<u32>),
}

impl PartialEq for StyleVal {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StyleVal::I(a), StyleVal::I(b)) => a == b,
            (StyleVal::F(a), StyleVal::F(b)) => a == b,
            (StyleVal::I(a), StyleVal::F(b)) | (StyleVal::F(b), StyleVal::I(a)) => Val::Int(*a as i128).eq_f64(*b),
            (StyleVal::G(a), StyleVal::G(b)) => a == b,
            _ => false,
        }
    }
}

/// Ordered like the Python dict (bit order).
#[derive(Clone, Debug, Default)]
pub struct Style(pub Vec<(&'static str, StyleVal)>);

impl Style {
    pub fn get(&self, key: &str) -> Option<&StyleVal> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }
    pub fn set(&mut self, key: &'static str, v: StyleVal) {
        if let Some(e) = self.0.iter_mut().find(|(k, _)| *k == key) {
            e.1 = v;
        } else {
            self.0.push((key, v));
        }
    }
    pub fn int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(StyleVal::I(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn float(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(StyleVal::F(v)) => Some(*v),
            Some(StyleVal::I(v)) => Some(*v as f64),
            _ => None,
        }
    }
}

impl PartialEq for Style {
    /// dict equality (order-independent)
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

fn read_fmt(data: &[u8], p: usize, fmt: Fmt) -> StyleVal {
    match fmt {
        Fmt::I32 => StyleVal::I(i32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as i64),
        Fmt::U32 => StyleVal::I(u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as i64),
        Fmt::F32 => StyleVal::F(f32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as f64),
        Fmt::U8 => StyleVal::I(data[p] as i64),
        Fmt::U16 => StyleVal::I(u16::from_le_bytes([data[p], data[p + 1]]) as i64),
    }
}

/// ink_editor.read_stroke_style on the stroke frame.
pub fn read_stroke_style(frame: &Frame) -> R<Style> {
    let mask = mask64(&frame.fields).0;
    let data = py_slice(&frame.raw, frame.flexible_start, None);
    let mut p = 0usize;
    let mut result = Style::default();
    let take = |p: &mut usize, n: usize| -> R<usize> {
        if *p + n > data.len() {
            return ink("truncated stroke style");
        }
        let at = *p;
        *p += n;
        Ok(at)
    };
    for bit in 0..64u32 {
        if (mask >> bit) & 1 == 0 {
            continue;
        }
        if bit == 24 {
            let at = take(&mut p, 2)?;
            let n = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
            if n > 256 {
                return ink("gradient color count bound");
            }
            let mut colors = Vec::with_capacity(n);
            for _ in 0..n {
                let at = take(&mut p, 4)?;
                colors.push(u32::from_le_bytes(data[at..at + 4].try_into().unwrap()));
            }
            result.set("gradientColorsARGB", StyleVal::G(colors));
        } else if let Some((name, fmt)) = style_field(bit) {
            let at = take(&mut p, fmt.size())?;
            let v = read_fmt(data, at, fmt);
            if let StyleVal::F(f) = v {
                if !f.is_finite() {
                    return ink("non-finite brush parameter");
                }
            }
            result.set(name, v);
        } else {
            return ink("unsupported stroke style field");
        }
    }
    if p != data.len() {
        return ink("unresolved trailing style bytes");
    }
    Ok(result)
}
