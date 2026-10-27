//! Python-compatible value model and primitives used by the editor port:
//! a JSON-like value tree with Python equality, `int()`/`float()` string
//! parsing, `float.__repr__`, `str.isspace`, strict base64 and `json.dumps`.
//! Pure Rust (no PyO3 types).
use std::cmp::Ordering;

/// Python `str.isspace` for one character.
pub fn py_isspace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ' | '\u{85}' | '\u{a0}'
        | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// Python `str.strip()` without arguments.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python integer kept as its canonical decimal representation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PyInt(pub String);

impl PyInt {
    pub fn from_i128(v: i128) -> Self {
        PyInt(v.to_string())
    }
    pub fn to_i128(&self) -> Option<i128> {
        self.0.parse().ok()
    }
    pub fn to_i64(&self) -> Option<i64> {
        self.0.parse().ok()
    }
    fn negative(&self) -> bool {
        self.0.starts_with('-')
    }
    /// Compare with a machine integer, exact for any size.
    pub fn cmp_i128(&self, other: i128) -> Ordering {
        match self.to_i128() {
            Some(v) => v.cmp(&other),
            None => if self.negative() { Ordering::Less } else { Ordering::Greater },
        }
    }
    pub fn to_f64(&self) -> f64 {
        self.0.parse::<f64>().unwrap_or(f64::NAN)
    }
}

/// Python `int(text)` (base 10) for ASCII digits. None means ValueError.
pub fn parse_py_int(text: &str) -> Option<PyInt> {
    let s = py_strip(text);
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let bytes = body.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() || !bytes[bytes.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut digits = String::with_capacity(bytes.len());
    let mut previous_underscore = false;
    for &b in bytes {
        if b == b'_' {
            if previous_underscore { return None; }
            previous_underscore = true;
        } else if b.is_ascii_digit() {
            previous_underscore = false;
            digits.push(b as char);
        } else {
            return None;
        }
    }
    let trimmed = digits.trim_start_matches('0');
    let canonical = if trimmed.is_empty() { "0".to_string() }
        else if neg { format!("-{trimmed}") } else { trimmed.to_string() };
    Some(PyInt(canonical))
}

/// Python `float(text)` for ASCII spellings. None means ValueError.
pub fn parse_py_float(text: &str) -> Option<f64> {
    let s = py_strip(text);
    if s.is_empty() { return None; }
    let chars: Vec<char> = s.chars().collect();
    let mut clean = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let ok = i > 0 && chars[i - 1].is_ascii_digit() && i + 1 < chars.len() && chars[i + 1].is_ascii_digit();
            if !ok { return None; }
            continue;
        }
        if !c.is_ascii() { return None; }
        clean.push(c);
    }
    let lower = clean.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if lower.len() - unsigned.len() > 1 { return None; }
    match unsigned {
        "inf" | "infinity" | "nan" => {}
        _ => {
            // Rust also accepts "infinity"/"nan" forms; everything else must be
            // a plain decimal literal (digits, one dot, optional exponent).
            if !unsigned.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-')) {
                return None;
            }
        }
    }
    clean.parse::<f64>().ok()
}

/// Python `float.__repr__`.
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() { return "nan".into(); }
    if x.is_infinite() { return if x > 0.0 { "inf".into() } else { "-inf".into() }; }
    if x == 0.0 { return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() }; }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let decpt = exponent + 1;
    let mut out = String::new();
    if x < 0.0 { out.push('-'); }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 { out.push('.'); out.push_str(&digits[1..]); }
        let e = decpt - 1;
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", e.abs()));
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) { out.push('0'); }
        out.push_str(&digits);
    } else if (decpt as usize) >= digits.len() {
        out.push_str(&digits);
        for _ in 0..(decpt as usize - digits.len()) { out.push('0'); }
        out.push_str(".0");
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    out
}

/// Python `base64.b64decode(s, validate=True)` for an already ASCII-checked
/// string (binascii strict mode). None means binascii.Error/ValueError.
pub fn b64decode_strict(s: &str) -> Option<Vec<u8>> {
    let data = s.as_bytes();
    if !s.is_ascii() { return None; }
    if data.first() == Some(&b'=') { return None; }
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let mut quad = 0u8;
    let mut left = 0u8;
    let mut pads = 0u32;
    let mut padding = false;
    for (i, &ch) in data.iter().enumerate() {
        if ch == b'=' {
            padding = true;
            if quad == 0 { return None; }
            if quad >= 2 {
                pads += 1;
                if quad as u32 + pads >= 4 {
                    if i + 1 < data.len() { return None; }
                    return Some(out);
                }
            }
            continue;
        }
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        if padding { return None; }
        pads = 0;
        match quad {
            0 => { quad = 1; left = v; }
            1 => { quad = 2; out.push((left << 2) | (v >> 4)); left = v & 0x0f; }
            2 => { quad = 3; out.push((left << 4) | (v >> 2)); left = v & 0x03; }
            _ => { quad = 0; out.push((left << 6) | v); left = 0; }
        }
    }
    if quad != 0 { return None; }
    Some(out)
}

pub fn b64encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// A Python value as seen by the model validators (subset of builtins).
#[derive(Clone, Debug)]
pub enum Val {
    None,
    Bool(bool),
    /// An int; `exact` is false for int subclasses (`type(x) is int` fails).
    Int(PyInt, bool),
    Float(f64),
    Str(String),
    /// A str that cannot be encoded to UTF-8 (lone surrogates); its length.
    BadStr(usize),
    List(Vec<Val>),
    Dict(Vec<(Val, Val)>),
    /// Any other object; `float(obj)` when that succeeds.
    Other(Option<f64>),
}

impl Val {
    pub fn int(v: i128) -> Val {
        Val::Int(PyInt::from_i128(v), true)
    }
    pub fn str(s: impl Into<String>) -> Val {
        Val::Str(s.into())
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Val::Str(s) = self { Some(s) } else { None }
    }
    /// `type(x) is int` and fits i64.
    pub fn exact_i64(&self) -> Option<i64> {
        if let Val::Int(v, true) = self { v.to_i64() } else { None }
    }
    pub fn is_exact_int(&self) -> bool {
        matches!(self, Val::Int(_, true))
    }
    pub fn dict(&self) -> Option<&Vec<(Val, Val)>> {
        if let Val::Dict(d) = self { Some(d) } else { None }
    }
    pub fn list(&self) -> Option<&Vec<Val>> {
        if let Val::List(l) = self { Some(l) } else { None }
    }
}

/// Look up a str key in a Python-like dict.
pub fn dget<'a>(d: &'a [(Val, Val)], key: &str) -> Option<&'a Val> {
    d.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

pub fn dget_mut<'a>(d: &'a mut [(Val, Val)], key: &str) -> Option<&'a mut Val> {
    d.iter_mut().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

pub fn dhas(d: &[(Val, Val)], key: &str) -> bool {
    dget(d, key).is_some()
}

/// Python `set(d) - allowed` is non-empty.
pub fn extra_keys(d: &[(Val, Val)], allowed: &[&str]) -> bool {
    d.iter().any(|(k, _)| !matches!(k.as_str(), Some(s) if allowed.contains(&s)))
}

/// Python `set(d) == expected`.
pub fn same_keys(d: &[(Val, Val)], expected: &[&str]) -> bool {
    !extra_keys(d, expected) && expected.iter().all(|k| dhas(d, k))
}

fn numeric(v: &Val) -> Option<f64> {
    match v {
        Val::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Val::Int(i, _) => Some(i.to_f64()),
        Val::Float(f) => Some(*f),
        _ => None,
    }
}

/// Python `==` for the supported value kinds (dicts compare unordered).
pub fn py_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::None, Val::None) => true,
        (Val::Str(x), Val::Str(y)) => x == y,
        (Val::Int(x, _), Val::Int(y, _)) => x == y,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Bool(_) | Val::Int(..), Val::Bool(_) | Val::Int(..)) => {
            let to = |v: &Val| match v { Val::Bool(b) => PyInt::from_i128(*b as i128), Val::Int(i, _) => i.clone(), _ => unreachable!() };
            to(a) == to(b)
        }
        (Val::Float(_), Val::Bool(_) | Val::Int(..) | Val::Float(_)) | (Val::Bool(_) | Val::Int(..), Val::Float(_)) => {
            numeric(a) == numeric(b)
        }
        (Val::List(x), Val::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Val::Dict(x), Val::Dict(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| py_eq(k, k2) && py_eq(v, v2)))
        }
        _ => false,
    }
}

// ---- json.dumps -----------------------------------------------------------

fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
}

/// `json.dumps(v, ensure_ascii=True, indent=indent, sort_keys=sort_keys,
/// separators=(',', ':') if compact)`. Only str dict keys are supported.
pub fn json_dumps(v: &Val, indent: Option<usize>, sort_keys: bool) -> String {
    let mut out = String::new();
    dump(v, indent, sort_keys, 0, &mut out);
    out
}

fn dump(v: &Val, indent: Option<usize>, sort_keys: bool, level: usize, out: &mut String) {
    match v {
        Val::None => out.push_str("null"),
        Val::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Val::Int(i, _) => out.push_str(&i.0),
        Val::Float(f) => {
            if f.is_nan() { out.push_str("NaN") }
            else if f.is_infinite() { out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" }) }
            else { out.push_str(&py_float_repr(*f)) }
        }
        Val::Str(s) => json_string(s, out),
        Val::BadStr(_) | Val::Other(_) => out.push_str("null"),
        Val::List(items) => {
            if items.is_empty() { out.push_str("[]"); return; }
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 { out.push(','); }
                newline(indent, level + 1, out);
                dump(item, indent, sort_keys, level + 1, out);
            }
            newline(indent, level, out);
            out.push(']');
        }
        Val::Dict(items) => {
            if items.is_empty() { out.push_str("{}"); return; }
            let mut refs: Vec<&(Val, Val)> = items.iter().collect();
            if sort_keys {
                refs.sort_by(|a, b| a.0.as_str().unwrap_or("").cmp(b.0.as_str().unwrap_or("")));
            }
            out.push('{');
            for (n, (k, item)) in refs.into_iter().enumerate() {
                if n > 0 { out.push(','); }
                newline(indent, level + 1, out);
                json_string(k.as_str().unwrap_or(""), out);
                out.push_str(if indent.is_some() { ": " } else { ":" });
                dump(item, indent, sort_keys, level + 1, out);
            }
            newline(indent, level, out);
            out.push('}');
        }
    }
}

fn newline(indent: Option<usize>, level: usize, out: &mut String) {
    if let Some(width) = indent {
        out.push('\n');
        for _ in 0..width * level { out.push(' '); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_repr() {
        for (x, s) in [(17.0, "17.0"), (0.1, "0.1"), (1e16, "1e+16"), (1e15, "1000000000000000.0"),
                       (0.0001, "0.0001"), (0.00001, "1e-05"), (24.5, "24.5"), (1.5e-7, "1.5e-07"), (1000.0, "1000.0")] {
            assert_eq!(py_float_repr(x), s);
        }
    }
    #[test]
    fn ints() {
        assert_eq!(parse_py_int(" +0_07 ").unwrap().0, "7");
        assert_eq!(parse_py_int("-0").unwrap().0, "0");
        assert!(parse_py_int("1__0").is_none());
        assert!(parse_py_int("_1").is_none());
        assert!(parse_py_int("").is_none());
    }
}
