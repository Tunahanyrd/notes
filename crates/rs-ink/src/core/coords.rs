//! ink_coordinates: native stored-owner scale and recipe-3 style override.
use super::binary::{f32_at, i32_at, mask64, put, py_slice, read_stroke_style, style_field, Frame, Fmt};
use super::num::{f32r, Val};
use super::{ink, InkErr, R};

pub const MIN_PEN_SIZE: f64 = 0.5;
pub const MAX_PEN_SIZE: f64 = 128.0;

/// ink_coordinates.stroke_owner_extent on the base frame.
pub fn stroke_owner_extent(base: &Frame) -> R<(i64, i64)> {
    let (fields, high) = mask64(&base.fields);
    let tail = base.raw.len() as i64 - base.flexible_start;
    let allowed = 0x6000u64 | 0x1 | 0x20 | 0x40000;
    if fields & 0x6000 != 0x6000 || high || fields & !allowed != 0 {
        return ink("unsupported stored owner metadata");
    }
    let after = if fields & 0x40000 != 0 { 16 } else { 0 };
    let before = tail - 16 - after;
    let minimum = if fields & 1 != 0 { 4 } else { 0 };
    if before < minimum || (fields & 0x20 == 0 && before != minimum) {
        return ink("unsupported stored owner metadata");
    }
    let off = base.flexible_start + before;
    // struct.unpack_from('<qii', raw, off): validate the whole 16-byte read.
    super::binary::i64_at(&base.raw, off)?;
    let width = i32_at(&base.raw, off + 8)? as i64;
    let height = i32_at(&base.raw, off + 12)? as i64;
    if !(1..=32768).contains(&width) || !(1..=32768).contains(&height) {
        return ink("invalid stored owner extent");
    }
    Ok((width, height))
}

/// ink_coordinates.page_coordinate_scale (orientation/extent already typed).
pub fn page_coordinate_scale(base: &Frame, width: Val, height: Val, orientation: Val) -> R<f64> {
    if !matches!(orientation, Val::Int(0)) {
        return ink("page orientation conversion unsupported");
    }
    for v in [width, height] {
        match v {
            Val::Int(i) if (1..=32768).contains(&i) => {}
            _ => return ink("invalid page coordinate extent"),
        }
    }
    let (old_width, _) = stroke_owner_extent(base)?;
    f32r(width.f32v()? / f32r(old_width as f64)?)
}

/// ink_coordinates.rebase_style_bytes: scale reader fields 3/8/13 only.
pub fn rebase_style_bytes(base: &Frame, stroke: &Frame, width: Val, height: Val) -> R<Vec<u8>> {
    read_stroke_style(stroke)?;
    let scale = page_coordinate_scale(base, width, height, Val::Int(0))?;
    let mut data = py_slice(&stroke.raw, stroke.flexible_start, None).to_vec();
    let mask = mask64(&stroke.fields).0;
    let mut offset = 0usize;
    for bit in 0..64u32 {
        if (mask >> bit) & 1 == 0 {
            continue;
        }
        if bit == 24 {
            let count = super::binary::u16_at(&data, offset as i64)? as usize;
            offset += 2 + count * 4;
        } else {
            let (_, fmt) = style_field(bit).ok_or_else(|| InkErr::KeyError(bit.to_string()))?;
            if matches!(bit, 3 | 8 | 13) {
                let v = f32r(f32_at(&data, offset as i64)? * scale)?;
                put(&mut data, offset, &(v as f32).to_le_bytes())?;
            }
            offset += fmt.size();
        }
    }
    Ok(data)
}

/// ink_coordinates.override_style_bytes (recipe 3).
pub fn override_style_bytes(stroke: &Frame, data: &[u8], color: Val, pen_size: Val) -> R<Vec<u8>> {
    check_override_args(color, pen_size)?;
    override_checked(stroke, data, color, pen_size)
}

/// The argument checks override_style_bytes performs before touching the record.
pub fn check_override_args(color: Val, pen_size: Val) -> R<()> {
    match color {
        Val::Int(c) if (0..=0xffffffff).contains(&c) && c >> 24 == 0xff => {}
        _ => return ink("opaque ARGB colour required"),
    }
    let size_ok = match pen_size {
        Val::Int(i) => (i as f64) >= MIN_PEN_SIZE && (i as f64) <= MAX_PEN_SIZE,
        Val::Float(f) => f.is_finite() && (MIN_PEN_SIZE..=MAX_PEN_SIZE).contains(&f),
        _ => false,
    };
    if !size_ok {
        return ink("pen size out of range");
    }
    Ok(())
}

fn override_checked(stroke: &Frame, data: &[u8], color: Val, pen_size: Val) -> R<Vec<u8>> {
    let mask = mask64(&stroke.fields).0;
    if (mask >> 2) & 1 == 0 || (mask >> 3) & 1 == 0 {
        return ink("template lacks colour or size field");
    }
    let mut data = data.to_vec();
    let mut offsets: [Option<usize>; 64] = [None; 64];
    let mut offset = 0usize;
    for bit in 0..64u32 {
        if (mask >> bit) & 1 == 0 {
            continue;
        }
        if bit == 24 {
            let count = super::binary::u16_at(&data, offset as i64)? as usize;
            offset += 2 + count * 4;
        } else {
            let (_, fmt) = style_field(bit).ok_or_else(|| InkErr::KeyError(bit.to_string()))?;
            offsets[bit as usize] = Some(offset);
            offset += fmt.size();
        }
    }
    if offset != data.len() {
        return ink("unresolved trailing style bytes");
    }
    let o3 = offsets[3].unwrap();
    let old = f32_at(&data, o3 as i64)?;
    if !(old > 0.0) {
        return ink("template pen size invalid");
    }
    let size = pen_size.f32v()?;
    let factor = size / old;
    let c = if let Val::Int(c) = color { c as u32 } else { unreachable!() };
    put(&mut data, offsets[2].unwrap(), &c.to_le_bytes())?;
    put(&mut data, o3, &(size as f32).to_le_bytes())?;
    if let Some(o8) = offsets[8] {
        let fixed = f32_at(&data, o8 as i64)?;
        let v = if fixed == old { size } else { f32r(fixed * factor)? };
        put(&mut data, o8, &(v as f32).to_le_bytes())?;
    }
    if let Some(o13) = offsets[13] {
        let v = f32r(f32_at(&data, o13 as i64)? * factor)?;
        put(&mut data, o13, &(v as f32).to_le_bytes())?;
    }
    let _ = Fmt::F32;
    Ok(data)
}
