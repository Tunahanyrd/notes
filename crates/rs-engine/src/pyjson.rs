//! JSON exactly as CPython's `json` module reads and writes it.
//!
//! The loopback API is specified by `json.loads(raw, object_pairs_hook=unique,
//! parse_constant=reject)` and `json.dumps(value, ensure_ascii=False,
//! allow_nan=False)`. That differs from serde_json in ways the UI and tests can
//! observe: lone `\udXXX` escapes are accepted (and later rejected by the store
//! with its own error), integers have arbitrary precision (but at most 4300
//! digits), UTF-16/32 bodies are detected, duplicate keys are an error, and the
//! output keeps `", "`/`": "` separators and Python float repr.
//! Pure Rust: no PyO3 types here.

/// A JSON string. `Wide` keeps UTF-16 code units when the text contains lone
/// surrogates (valid Python `str`, not representable as a Rust `String`).
#[derive(Clone, Debug, PartialEq)]
pub enum JStr {
    Utf8(String),
    Wide(Vec<u16>),
}

impl JStr {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JStr::Utf8(s) => Some(s),
            JStr::Wide(_) => None,
        }
    }
    /// Python `len(str)` (code points; a lone surrogate counts as one).
    pub fn char_len(&self) -> usize {
        match self {
            JStr::Utf8(s) => s.chars().count(),
            JStr::Wide(units) => char::decode_utf16(units.iter().copied()).count(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum JValue {
    Null,
    Bool(bool),
    /// Decimal integer literal text (sign included), arbitrary precision.
    Int(String),
    /// Float literal text as written (converted with Python `float()` semantics).
    Float(String),
    Str(JStr),
    Array(Vec<JValue>),
    /// Insertion-ordered, keys unique (duplicates are rejected while parsing).
    Object(Vec<(JStr, JValue)>),
}

impl JValue {
    pub fn str(s: &str) -> JValue {
        JValue::Str(JStr::Utf8(s.to_owned()))
    }
    pub fn object(fields: Vec<(&str, JValue)>) -> JValue {
        JValue::Object(fields.into_iter().map(|(k, v)| (JStr::Utf8(k.to_owned()), v)).collect())
    }
    /// `dict.get(key)` for objects.
    pub fn get(&self, key: &str) -> Option<&JValue> {
        match self {
            JValue::Object(fields) => fields.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }
    /// The object's key set as UTF-8 strings (`None` keys are surrogate keys).
    pub fn keys(&self) -> Vec<Option<&str>> {
        match self {
            JValue::Object(fields) => fields.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
    /// Python `set(data) == {names...}` for a JSON object.
    pub fn keys_equal(&self, names: &[&str]) -> bool {
        let keys = self.keys();
        keys.len() == names.len() && names.iter().all(|n| keys.contains(&Some(*n)))
    }
    /// Python `set(data) <= {names...}`.
    pub fn keys_subset(&self, names: &[&str]) -> bool {
        self.keys().iter().all(|k| matches!(k, Some(k) if names.contains(k)))
    }
}

/// Maximum digits CPython converts from a decimal string (sys.int_info default).
pub const MAX_INT_DIGITS: usize = 4300;
/// Nesting depth accepted for local API bodies (CPython's scanner raised
/// RecursionError only near ~60k frames). The parser is iterative, but
/// `dumps`, the engine's JSON conversion and `Drop` recurse once per level:
/// a 60k-deep body overflowed even the 64 MiB connection thread and aborted
/// the whole engine. Every API body schema is a few levels deep.
pub const MAX_DEPTH: usize = 4_096;

/// Decode bytes like `json.loads(bytes)`: encoding detection + surrogatepass.
fn decode_text(b: &[u8]) -> Option<Vec<u32>> {
    const BOM32_BE: &[u8] = &[0, 0, 0xfe, 0xff];
    const BOM32_LE: &[u8] = &[0xff, 0xfe, 0, 0];
    if b.starts_with(BOM32_BE) {
        return decode_utf32(&b[4..], true);
    }
    if b.starts_with(BOM32_LE) {
        return decode_utf32(&b[4..], false);
    }
    if b.starts_with(&[0xfe, 0xff]) {
        return decode_utf16(&b[2..], true);
    }
    if b.starts_with(&[0xff, 0xfe]) {
        return decode_utf16(&b[2..], false);
    }
    if b.starts_with(&[0xef, 0xbb, 0xbf]) {
        return decode_utf8_surrogatepass(&b[3..]);
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 { decode_utf16(b, true) } else { decode_utf32(b, true) };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 { decode_utf16(b, false) } else { decode_utf32(b, false) };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return decode_utf16(b, true);
        }
        if b[1] == 0 {
            return decode_utf16(b, false);
        }
    }
    decode_utf8_surrogatepass(b)
}

fn decode_utf32(b: &[u8], be: bool) -> Option<Vec<u32>> {
    if b.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4);
    for c in b.chunks(4) {
        let v = if be {
            u32::from_be_bytes([c[0], c[1], c[2], c[3]])
        } else {
            u32::from_le_bytes([c[0], c[1], c[2], c[3]])
        };
        if v > 0x10ffff {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

fn decode_utf16(b: &[u8], be: bool) -> Option<Vec<u32>> {
    if b.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = b
        .chunks(2)
        .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
        .collect();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i] as u32;
        if (0xd800..0xdc00).contains(&u) && i + 1 < units.len() && (0xdc00..0xe000).contains(&(units[i + 1] as u32)) {
            out.push(0x10000 + ((u - 0xd800) << 10) + (units[i + 1] as u32 - 0xdc00));
            i += 2;
        } else {
            out.push(u); // surrogatepass keeps lone surrogates
            i += 1;
        }
    }
    Some(out)
}

/// UTF-8 with Python's `surrogatepass` (encoded surrogates ED A0..BF xx allowed).
fn decode_utf8_surrogatepass(b: &[u8]) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            out.push(c as u32);
            i += 1;
            continue;
        }
        let (n, min, init) = match c {
            0xc2..=0xdf => (1, 0x80, (c & 0x1f) as u32),
            0xe0..=0xef => (2, 0x800, (c & 0x0f) as u32),
            0xf0..=0xf4 => (3, 0x10000, (c & 0x07) as u32),
            _ => return None,
        };
        let mut v = init;
        for k in 1..=n {
            let cc = *b.get(i + k)?;
            if cc & 0xc0 != 0x80 {
                return None;
            }
            v = (v << 6) | (cc & 0x3f) as u32;
        }
        if v < min || v > 0x10ffff {
            return None;
        }
        out.push(v);
        i += n + 1;
    }
    Some(out)
}

struct Parser<'a> {
    s: &'a [u32],
    i: usize,
    /// API mode: duplicate keys and NaN/Infinity are errors. Lenient mode is
    /// plain `json.loads` (last duplicate wins in the first position).
    strict: bool,
}

#[inline]
fn is_ws(c: u32) -> bool {
    c == 0x20 || c == 0x09 || c == 0x0a || c == 0x0d
}

fn hex4(s: &[u32], at: usize) -> Option<u32> {
    if at + 4 > s.len() {
        return None;
    }
    let mut v = 0u32;
    for &c in &s[at..at + 4] {
        let d = char::from_u32(c)?.to_digit(16)?;
        if !(c < 0x80) {
            return None;
        }
        v = v * 16 + d;
    }
    Some(v)
}

fn make_jstr(units: Vec<u32>) -> JStr {
    if units.iter().all(|&c| !(0xd800..0xe000).contains(&c)) {
        JStr::Utf8(units.into_iter().map(|c| char::from_u32(c).unwrap_or('\u{fffd}')).collect())
    } else {
        let mut wide = Vec::with_capacity(units.len());
        for c in units {
            if c >= 0x10000 {
                let v = c - 0x10000;
                wide.push(0xd800 + (v >> 10) as u16);
                wide.push(0xdc00 + (v & 0x3ff) as u16);
            } else {
                wide.push(c as u16);
            }
        }
        JStr::Wide(wide)
    }
}

enum Frame {
    Array(Vec<JValue>),
    Object(Vec<(JStr, JValue)>, Option<JStr>),
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && is_ws(self.s[self.i]) {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u32> {
        self.s.get(self.i).copied()
    }
    fn lit(&mut self, word: &str) -> bool {
        let w: Vec<u32> = word.chars().map(|c| c as u32).collect();
        if self.s.len() >= self.i + w.len() && self.s[self.i..self.i + w.len()] == w[..] {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    /// `scanstring` (strict=True); self.i points just after the opening quote.
    fn string(&mut self) -> Result<JStr, ()> {
        let mut out: Vec<u32> = Vec::new();
        loop {
            let c = *self.s.get(self.i).ok_or(())?;
            self.i += 1;
            match c {
                0x22 => return Ok(make_jstr(out)),
                0x5c => {
                    let e = *self.s.get(self.i).ok_or(())?;
                    self.i += 1;
                    let mapped = match e {
                        0x22 => 0x22,
                        0x5c => 0x5c,
                        0x2f => 0x2f,
                        0x62 => 0x08,
                        0x66 => 0x0c,
                        0x6e => 0x0a,
                        0x72 => 0x0d,
                        0x74 => 0x09,
                        0x75 => {
                            let mut v = hex4(self.s, self.i).ok_or(())?;
                            self.i += 4;
                            if (0xd800..0xdc00).contains(&v)
                                && self.i + 1 < self.s.len()
                                && self.s[self.i] == 0x5c
                                && self.s[self.i + 1] == 0x75
                            {
                                let low = hex4(self.s, self.i + 2).ok_or(())?;
                                if (0xdc00..0xe000).contains(&low) {
                                    v = 0x10000 + ((v - 0xd800) << 10) + (low - 0xdc00);
                                    self.i += 6;
                                }
                            }
                            v
                        }
                        _ => return Err(()),
                    };
                    out.push(mapped);
                }
                c if c < 0x20 => return Err(()),
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<JValue, ()> {
        let start = self.i;
        let n = self.s.len();
        let digit = |c: u32| (0x30..=0x39).contains(&c);
        if self.s[self.i] == 0x2d {
            self.i += 1;
            if self.i >= n {
                return Err(());
            }
        }
        if (0x31..=0x39).contains(&self.s[self.i]) {
            self.i += 1;
            while self.i < n && digit(self.s[self.i]) {
                self.i += 1;
            }
        } else if self.s[self.i] == 0x30 {
            self.i += 1;
        } else {
            return Err(());
        }
        let mut float = false;
        if self.i + 1 < n && self.s[self.i] == 0x2e && digit(self.s[self.i + 1]) {
            float = true;
            self.i += 2;
            while self.i < n && digit(self.s[self.i]) {
                self.i += 1;
            }
        }
        if self.i < n && (self.s[self.i] == 0x65 || self.s[self.i] == 0x45) {
            let e_start = self.i;
            self.i += 1;
            if self.i < n && (self.s[self.i] == 0x2b || self.s[self.i] == 0x2d) {
                self.i += 1;
            }
            let digits_start = self.i;
            while self.i < n && digit(self.s[self.i]) {
                self.i += 1;
            }
            if self.i > digits_start {
                float = true;
            } else {
                self.i = e_start;
            }
        }
        let text: String = self.s[start..self.i].iter().map(|&c| c as u8 as char).collect();
        if float {
            Ok(JValue::Float(text))
        } else {
            let digits = text.trim_start_matches('-').len();
            if digits > MAX_INT_DIGITS {
                return Err(()); // ValueError: exceeds the integer string conversion limit
            }
            Ok(JValue::Int(text))
        }
    }

    /// One scalar, or the opening of a container (pushed onto `stack`).
    fn value(&mut self, stack: &mut Vec<Frame>) -> Result<Option<JValue>, ()> {
        let c = self.peek().ok_or(())?;
        match c {
            0x22 => {
                self.i += 1;
                Ok(Some(JValue::Str(self.string()?)))
            }
            0x7b => {
                self.i += 1;
                if stack.len() >= MAX_DEPTH {
                    return Err(());
                }
                stack.push(Frame::Object(Vec::new(), None));
                Ok(None)
            }
            0x5b => {
                self.i += 1;
                if stack.len() >= MAX_DEPTH {
                    return Err(());
                }
                stack.push(Frame::Array(Vec::new()));
                Ok(None)
            }
            0x6e if self.lit("null") => Ok(Some(JValue::Null)),
            0x74 if self.lit("true") => Ok(Some(JValue::Bool(true))),
            0x66 if self.lit("false") => Ok(Some(JValue::Bool(false))),
            // parse_constant rejects NaN/Infinity (ValueError -> invalid_json).
            0x4e if self.lit("NaN") => if self.strict { Err(()) } else { Ok(Some(JValue::Float("NaN".into()))) },
            0x49 if self.lit("Infinity") => if self.strict { Err(()) } else { Ok(Some(JValue::Float("Infinity".into()))) },
            0x2d if self.lit("-Infinity") => if self.strict { Err(()) } else { Ok(Some(JValue::Float("-Infinity".into()))) },
            0x2d | 0x30..=0x39 => Ok(Some(self.number()?)),
            _ => Err(()),
        }
    }

    fn object_key(&mut self, frame_fields: &[(JStr, JValue)]) -> Result<JStr, ()> {
        // After '{' or ',': whitespace, then a quoted key, ':'
        self.ws();
        if self.peek() != Some(0x22) {
            return Err(());
        }
        self.i += 1;
        let key = self.string()?;
        if self.strict && frame_fields.iter().any(|(k, _)| *k == key) {
            return Err(()); // duplicate JSON field
        }
        self.ws();
        if self.peek() != Some(0x3a) {
            return Err(());
        }
        self.i += 1;
        self.ws();
        Ok(key)
    }

    fn parse(&mut self) -> Result<JValue, ()> {
        let mut stack: Vec<Frame> = Vec::new();
        self.ws();
        let mut pending: Option<JValue>;
        // Begin with a value.
        pending = self.value(&mut stack)?;
        loop {
            // Containers just opened: handle empty or first element.
            if pending.is_none() {
                match stack.last_mut() {
                    Some(Frame::Array(_)) => {
                        self.ws();
                        if self.peek() == Some(0x5d) {
                            self.i += 1;
                            if let Some(Frame::Array(items)) = stack.pop() {
                                pending = Some(JValue::Array(items));
                            }
                        } else {
                            pending = self.value(&mut stack)?;
                            continue;
                        }
                    }
                    Some(Frame::Object(..)) => {
                        self.ws();
                        if self.peek() == Some(0x7d) {
                            self.i += 1;
                            if let Some(Frame::Object(f, _)) = stack.pop() {
                                pending = Some(JValue::Object(f));
                            }
                        } else {
                            // Python: "Expecting property name enclosed in double quotes"
                            let new_key = self.object_key(&[])?;
                            if let Some(Frame::Object(_, key)) = stack.last_mut() {
                                *key = Some(new_key);
                            }
                            pending = self.value(&mut stack)?;
                            continue;
                        }
                    }
                    None => return Err(()),
                }
            }
            let value = pending.take().unwrap();
            match stack.last_mut() {
                None => {
                    self.ws();
                    if self.i != self.s.len() {
                        return Err(()); // Extra data
                    }
                    return Ok(value);
                }
                Some(Frame::Array(items)) => {
                    items.push(value);
                    self.ws();
                    match self.peek() {
                        Some(0x2c) => {
                            self.i += 1;
                            self.ws();
                            pending = self.value(&mut stack)?;
                            continue;
                        }
                        Some(0x5d) => {
                            self.i += 1;
                            if let Some(Frame::Array(items)) = stack.pop() {
                                pending = Some(JValue::Array(items));
                            }
                        }
                        _ => return Err(()),
                    }
                }
                Some(Frame::Object(fields, key)) => {
                    let k = key.take().ok_or(())?;
                    match fields.iter_mut().find(|(existing, _)| *existing == k) {
                        Some(slot) => slot.1 = value,
                        None => fields.push((k, value)),
                    }
                    self.ws();
                    match self.peek() {
                        Some(0x2c) => {
                            self.i += 1;
                            let new_key = self.object_key(fields)?;
                            if let Some(Frame::Object(_, key)) = stack.last_mut() {
                                *key = Some(new_key);
                            }
                            pending = self.value(&mut stack)?;
                            continue;
                        }
                        Some(0x7d) => {
                            self.i += 1;
                            if let Some(Frame::Object(f, _)) = stack.pop() {
                                pending = Some(JValue::Object(f));
                            }
                        }
                        _ => return Err(()),
                    }
                }
            }
            // A container closed: `pending` holds it; loop to attach it.
            if pending.is_none() {
                return Err(());
            }
        }
    }
}

/// `json.loads(raw, object_pairs_hook=unique, parse_constant=reject)`.
/// Every failure is the API's `invalid_json` (ValueError/UnicodeError/RecursionError).
pub fn loads(raw: &[u8]) -> Result<JValue, ()> {
    let text = decode_text(raw).ok_or(())?;
    Parser { s: &text, i: 0, strict: true }.parse()
}

/// Plain `json.loads(bytes)` (duplicates allowed, NaN/Infinity accepted).
pub fn loads_lenient(raw: &[u8]) -> Result<JValue, ()> {
    let text = decode_text(raw).ok_or(())?;
    Parser { s: &text, i: 0, strict: false }.parse()
}

/// Parse already-decoded text (Python `json.loads(str)`; a leading BOM is an error).
pub fn loads_str(text: &str) -> Result<JValue, ()> {
    if text.starts_with('\u{feff}') {
        return Err(());
    }
    let units: Vec<u32> = text.chars().map(|c| c as u32).collect();
    Parser { s: &units, i: 0, strict: true }.parse()
}

/// Plain `json.loads(str)`.
pub fn loads_str_lenient(text: &str) -> Result<JValue, ()> {
    if text.starts_with('\u{feff}') {
        return Err(());
    }
    let units: Vec<u32> = text.chars().map(|c| c as u32).collect();
    Parser { s: &units, i: 0, strict: false }.parse()
}

/// Python `value == n` for a parsed JSON number/bool.
pub fn eq_int(value: Option<&JValue>, n: i64) -> bool {
    match value {
        Some(JValue::Int(t)) => t.parse::<i64>().ok() == Some(n),
        Some(JValue::Float(t)) => t.parse::<f64>().ok() == Some(n as f64),
        Some(JValue::Bool(b)) => (*b as i64) == n,
        _ => false,
    }
}

/// Python `repr(float)` for a finite value.
pub fn float_repr(v: f64) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Shortest round-trip digits via `{:e}`: "d.ddde±x".
    let sci = format!("{:e}", v);
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if negative { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let mut m = digits[..1].to_string();
        if digits.len() > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        format!("{}{}e{}{:02}", sign, m, if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if exp < 0 {
        format!("{}0.{}{}", sign, "0".repeat((-exp - 1) as usize), digits)
    } else {
        let point = exp as usize + 1;
        if digits.len() <= point {
            format!("{}{}{}.0", sign, digits, "0".repeat(point - digits.len()))
        } else {
            format!("{}{}.{}", sign, &digits[..point], &digits[point..])
        }
    }
}

fn dump_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(value, ensure_ascii=False, allow_nan=False).encode()`.
/// Errors where Python would (non-finite float, lone surrogate on encode).
pub fn dumps(value: &JValue) -> Result<Vec<u8>, ()> {
    let mut out = String::new();
    dump_into(&mut out, value)?;
    Ok(out.into_bytes())
}

fn dump_into(out: &mut String, value: &JValue) -> Result<(), ()> {
    match value {
        JValue::Null => out.push_str("null"),
        JValue::Bool(true) => out.push_str("true"),
        JValue::Bool(false) => out.push_str("false"),
        JValue::Int(text) => {
            // Python int repr normalizes "-0" to "0".
            if text.trim_start_matches('-') == "0" {
                out.push('0')
            } else {
                out.push_str(text)
            }
        }
        JValue::Float(text) => {
            let v: f64 = text.parse().map_err(|_| ())?;
            if !v.is_finite() {
                return Err(());
            }
            out.push_str(&float_repr(v));
        }
        JValue::Str(JStr::Utf8(s)) => dump_str(out, s),
        JValue::Str(JStr::Wide(_)) => return Err(()),
        JValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_into(out, item)?;
            }
            out.push(']');
        }
        JValue::Object(fields) => {
            out.push('{');
            for (i, (k, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match k {
                    JStr::Utf8(s) => dump_str(out, s),
                    JStr::Wide(_) => return Err(()),
                }
                out.push_str(": ");
                dump_into(out, v)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_compatible_parsing() {
        assert!(loads(b"{\"a\": 1, \"a\": 2}").is_err());
        assert!(loads(b"[NaN]").is_err());
        assert!(loads(b"[1,]").is_err());
        assert!(loads(b"{\"a\":1,}").is_err());
        assert!(loads(b"01").is_err());
        assert_eq!(loads(b" [1.5e3, -0, 1E2] ").unwrap(), JValue::Array(vec![
            JValue::Float("1.5e3".into()), JValue::Int("-0".into()), JValue::Float("1E2".into())]));
        assert_eq!(loads(b"\"\\ud800\"").unwrap(), JValue::Str(JStr::Wide(vec![0xd800])));
        assert_eq!(loads(b"\"\\ud83d\\ude80\"").unwrap(), JValue::str("\u{1f680}"));
        assert_eq!(loads(b"\xef\xbb\xbf{}").unwrap(), JValue::Object(vec![]));
        assert!(loads(b"\"a\x01\"").is_err());
        assert!(loads(b"{} x").is_err());
        assert!(loads(b"1e").is_err());
        assert_eq!(loads(b"[[[]], {\"k\": {\"x\": [true, null]}}]").unwrap(), JValue::Array(vec![
            JValue::Array(vec![JValue::Array(vec![])]),
            JValue::object(vec![("k", JValue::object(vec![("x", JValue::Array(vec![JValue::Bool(true), JValue::Null]))]))]),
        ]));
        let deep = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
        assert!(loads(deep.as_bytes()).is_ok());
    }

    #[test]
    fn python_float_repr() {
        for (v, s) in [(1.0, "1.0"), (0.1, "0.1"), (1e16, "1e+16"), (1e-5, "1e-05"), (123.456, "123.456"),
                       (1e15, "1000000000000000.0"), (0.0001, "0.0001"), (-2.5e-7, "-2.5e-07"), (1.5e300, "1.5e+300")] {
            assert_eq!(float_repr(v), s);
        }
        assert_eq!(dumps(&JValue::object(vec![("a", JValue::str("x\"\n\u{1}ü")), ("b", JValue::Array(vec![]))])).unwrap(),
                   "{\"a\": \"x\\\"\\n\\u0001ü\", \"b\": []}".as_bytes());
    }
}
