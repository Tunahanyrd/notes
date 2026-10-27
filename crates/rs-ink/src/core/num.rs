//! Python numeric semantics needed for byte-exact ports.
use super::{InkErr, R};

pub const UNREPRESENTABLE: &str = "non-finite or unrepresentable stroke value";

/// ink_editor._f32: round through IEEE float32, rejecting non-finite results.
pub fn f32r(v: f64) -> R<f64> {
    let y = v as f32;
    if !y.is_finite() {
        return Err(InkErr::Ink(UNREPRESENTABLE.into()));
    }
    Ok(y as f64)
}

/// A Python number as received from a caller, keeping the exact-type facts
/// the oracle checks (`type(v) in (int, float)` excludes bool and subclasses).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    Int(i128),
    /// An int outside i128 (sign only); converting it to float overflows.
    Big(bool),
    Float(f64),
    /// Any other type (bool, None, str, subclasses, ...).
    Bad,
}

impl Val {
    pub fn is_num(&self) -> bool {
        !matches!(self, Val::Bad)
    }
    pub fn is_int(&self) -> bool {
        matches!(self, Val::Int(_) | Val::Big(_))
    }
    /// float(v)
    pub fn to_f64(&self) -> R<f64> {
        match *self {
            Val::Int(i) => Ok(i as f64),
            Val::Float(f) => Ok(f),
            Val::Big(_) => Err(InkErr::Overflow("int too large to convert to float".into())),
            Val::Bad => Err(InkErr::TypeError("must be real number".into())),
        }
    }
    /// _f32(v): struct.pack('<f', v) errors all become InkError.
    pub fn f32v(&self) -> R<f64> {
        match *self {
            Val::Int(i) => f32r(i as f64),
            Val::Float(f) => f32r(f),
            _ => Err(InkErr::Ink(UNREPRESENTABLE.into())),
        }
    }
    /// math.isfinite(v)
    pub fn is_finite(&self) -> R<bool> {
        match *self {
            Val::Int(_) => Ok(true),
            Val::Float(f) => Ok(f.is_finite()),
            Val::Big(_) => Err(InkErr::Overflow("int too large to convert to float".into())),
            Val::Bad => Err(InkErr::TypeError("must be real number".into())),
        }
    }
    /// abs(v) > limit (exact for ints)
    pub fn abs_gt(&self, limit: f64) -> bool {
        match *self {
            Val::Int(i) => (i.unsigned_abs() as f64) > limit,
            Val::Float(f) => f.abs() > limit,
            Val::Big(_) => true,
            Val::Bad => false,
        }
    }
    /// Python `f == v` with exact int/float comparison.
    pub fn eq_f64(&self, f: f64) -> bool {
        match *self {
            Val::Float(v) => v == f,
            Val::Int(i) => f.is_finite() && f.fract() == 0.0 && f.abs() < 1.7e38 && (f as i128) == i,
            _ => false,
        }
    }
    pub fn is_zero(&self) -> bool {
        match *self {
            Val::Int(i) => i == 0,
            Val::Float(f) => f == 0.0,
            _ => false,
        }
    }
}

/// Python `min(a, b)`: the first argument unless the second is smaller.
pub fn py_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// Python `max(a, b)`: the first argument unless the second is larger.
pub fn py_max(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// Python `repr(float)` (shortest round-trip, Python exponent rules).
pub fn py_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Shortest correctly rounded digits that round-trip (Python repr / dtoa mode 0).
    let sci = (1..=17usize)
        .map(|p| format!("{:.*e}", p - 1, v))
        .find(|s| s.parse::<f64>().ok() == Some(v))
        .unwrap_or_else(|| format!("{:.16e}", v));
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let n = digits.len() as i32;
        let point = exp + 1; // digits before the decimal point
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point >= n {
            format!("{}{}.0", digits, "0".repeat((point - n) as usize))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        let es = if exp < 0 { "-" } else { "+" };
        format!("{sign}{m}e{es}{:02}", exp.abs())
    }
}

/// Python `float(text)` for str input.
pub fn py_float(text: &str) -> R<f64> {
    let err = || InkErr::ValueError(format!("could not convert string to float: {:?}", text));
    let t = text.trim();
    if t.is_empty() {
        return Err(err());
    }
    let cleaned: String = if t.contains('_') {
        let b: Vec<char> = t.chars().collect();
        for (i, c) in b.iter().enumerate() {
            if *c == '_' {
                let ok = i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit();
                if !ok {
                    return Err(err());
                }
            }
        }
        t.chars().filter(|c| *c != '_').collect()
    } else {
        t.to_string()
    };
    let lowered = cleaned.to_ascii_lowercase();
    let body = lowered.trim_start_matches(['+', '-']);
    if matches!(body, "inf" | "infinity" | "nan") {
        return cleaned.parse::<f64>().map_err(|_| err());
    }
    if !body.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-')) {
        return Err(err());
    }
    cleaned.parse::<f64>().map_err(|_| err())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repr_matches_python() {
        for (v, s) in [(1.0, "1.0"), (0.1, "0.1"), (1e16, "1e+16"), (1e-5, "1e-05"), (123.456, "123.456"),
                       (1.5e300, "1.5e+300"), (-2.5, "-2.5"), (9.0e15, "9000000000000000.0"), (0.0001, "0.0001"), (134.56875610351562, "134.56875610351562")] {
            assert_eq!(py_repr(v), s);
        }
    }
}
