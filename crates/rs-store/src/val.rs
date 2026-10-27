//! Python-compatible JSON values: the data model shared by the pure-Rust store
//! core and the PyO3 wrappers. `dumps`/`loads` reproduce CPython's `json`
//! module byte-for-byte (default separators, `ensure_ascii`, float `repr`).
use crate::casefold_table::CASEFOLD;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Int(i64),
    /// Python int outside i64, kept as its decimal text.
    BigInt(String),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Val>),
    Dict(Vec<(String, Val)>),
}

impl Val {
    pub fn s(v: impl Into<String>) -> Val {
        Val::Str(v.into())
    }
    pub fn dict(items: Vec<(&str, Val)>) -> Val {
        Val::Dict(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn opt_str(v: Option<&str>) -> Val {
        v.map(Val::s).unwrap_or(Val::Null)
    }
    pub fn get(&self, key: &str) -> Option<&Val> {
        match self {
            Val::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// dict.get(key) semantics: missing key -> None.
    pub fn get_or_null(&self, key: &str) -> Val {
        self.get(key).cloned().unwrap_or(Val::Null)
    }
    pub fn set(&mut self, key: &str, value: Val) {
        if let Val::Dict(items) = self {
            if let Some(slot) = items.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value;
            } else {
                items.push((key.to_string(), value));
            }
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<Val> {
        if let Val::Dict(items) = self {
            if let Some(at) = items.iter().position(|(k, _)| k == key) {
                return Some(items.remove(at).1);
            }
        }
        None
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Val::Null)
    }
    /// `type(v) is int` (bool excluded).
    pub fn is_exact_int(&self) -> bool {
        matches!(self, Val::Int(_) | Val::BigInt(_))
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            Val::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(*i as f64),
            Val::Bool(b) => Some(*b as i64 as f64),
            Val::Float(f) => Some(*f),
            Val::BigInt(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }
    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Val::Null => false,
            Val::Bool(b) => *b,
            Val::Int(i) => *i != 0,
            Val::BigInt(_) => true,
            Val::Float(f) => *f != 0.0,
            Val::Str(s) => !s.is_empty(),
            Val::Bytes(b) => !b.is_empty(),
            Val::List(l) => !l.is_empty(),
            Val::Dict(d) => !d.is_empty(),
        }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Val::Null => "NoneType",
            Val::Bool(_) => "bool",
            Val::Int(_) | Val::BigInt(_) => "int",
            Val::Float(_) => "float",
            Val::Str(_) => "str",
            Val::Bytes(_) => "bytes",
            Val::List(_) => "list",
            Val::Dict(_) => "dict",
        }
    }
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Val::Dict(items) => items.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

/// Python `==` for JSON-like values (numeric types compare by value).
pub fn py_eq(a: &Val, b: &Val) -> bool {
    use Val::*;
    match (a, b) {
        (Null, Null) => true,
        (Str(x), Str(y)) => x == y,
        (Bytes(x), Bytes(y)) => x == y,
        (BigInt(x), BigInt(y)) => x == y,
        (List(x), List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Dict(x), Dict(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && py_eq(v, v2)))
        }
        (Bool(_) | Int(_), Bool(_) | Int(_)) => a.as_i64() == b.as_i64(),
        (Float(x), Float(y)) => x == y,
        (Float(f), Bool(_) | Int(_)) | (Bool(_) | Int(_), Float(f)) => {
            let i = if let Float(_) = a { b.as_i64() } else { a.as_i64() }.unwrap();
            // exact comparison like CPython
            f.is_finite() && f.fract() == 0.0 && *f >= -9.3e18 && *f <= 9.3e18 && (*f as i128) == i as i128
        }
        _ => false,
    }
}

/// CPython `str.casefold()`.
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x80 {
            out.push(c.to_ascii_lowercase());
            continue;
        }
        match CASEFOLD.binary_search_by_key(&cp, |e| e.0) {
            Ok(at) => out.push_str(CASEFOLD[at].1),
            Err(_) => out.push(c),
        }
    }
    out
}

/// CPython `float.__repr__`.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Rust's `{:e}` is the shortest round-trip representation: d[.ddd]e<exp>
    let sci = format!("{:e}", f);
    let (neg, sci) = match sci.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, sci),
    };
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1; // value = 0.DIGITS * 10^decpt
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if decpt > -4 && decpt <= 16 {
        let n = digits.len() as i32;
        if decpt <= 0 {
            out.push_str("0.");
            for _ in 0..(-decpt) {
                out.push('0');
            }
            out.push_str(&digits);
        } else if decpt >= n {
            out.push_str(&digits);
            for _ in 0..(decpt - n) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt as usize]);
            out.push('.');
            out.push_str(&digits[decpt as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        let _ = write!(out, "e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    out
}

#[derive(Debug)]
pub enum JsonError {
    /// ValueError (e.g. out-of-range float with allow_nan=False)
    Value(String),
    /// TypeError (non-serializable object)
    Type(String),
    /// json.JSONDecodeError (a ValueError)
    Decode(String),
}

fn escape_into(out: &mut String, s: &str, ensure_ascii: bool) {
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
            c if (c as u32) < 0x20 || (ensure_ascii && (c as u32) > 0x7e) => {
                let cp = c as u32;
                if cp >= 0x10000 {
                    let v = cp - 0x10000;
                    let _ = write!(out, "\\u{:04x}\\u{:04x}", 0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF));
                } else {
                    let _ = write!(out, "\\u{:04x}", cp);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn encode_into(out: &mut String, v: &Val, ensure_ascii: bool, allow_nan: bool) -> Result<(), JsonError> {
    match v {
        Val::Null => out.push_str("null"),
        Val::Bool(true) => out.push_str("true"),
        Val::Bool(false) => out.push_str("false"),
        Val::Int(i) => {
            let _ = write!(out, "{}", i);
        }
        Val::BigInt(s) => out.push_str(s),
        Val::Float(f) => {
            if f.is_finite() {
                out.push_str(&float_repr(*f));
            } else if !allow_nan {
                return Err(JsonError::Value(format!(
                    "Out of range float values are not JSON compliant: {}",
                    float_repr(*f)
                )));
            } else if f.is_nan() {
                out.push_str("NaN");
            } else if *f > 0.0 {
                out.push_str("Infinity");
            } else {
                out.push_str("-Infinity");
            }
        }
        Val::Str(s) => escape_into(out, s, ensure_ascii),
        Val::Bytes(_) => return Err(JsonError::Type("Object of type bytes is not JSON serializable".into())),
        Val::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                encode_into(out, item, ensure_ascii, allow_nan)?;
            }
            out.push(']');
        }
        Val::Dict(items) => {
            out.push('{');
            for (i, (k, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                escape_into(out, k, ensure_ascii);
                out.push_str(": ");
                encode_into(out, item, ensure_ascii, allow_nan)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// `json.dumps(v, ensure_ascii=..., allow_nan=...)` with default separators.
pub fn dumps(v: &Val, ensure_ascii: bool, allow_nan: bool) -> Result<String, JsonError> {
    let mut out = String::new();
    encode_into(&mut out, v, ensure_ascii, allow_nan)?;
    Ok(out)
}

/// `json.dumps(v)` (ensure_ascii=True).
pub fn dumps_ascii(v: &Val) -> Result<String, JsonError> {
    dumps(v, true, true)
}

/// `json.dumps(v, ensure_ascii=False)`.
pub fn dumps_utf8(v: &Val) -> Result<String, JsonError> {
    dumps(v, false, true)
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
    depth: usize,
}

/// Container nesting accepted by [`loads`] (CPython's json raises
/// RecursionError near its default recursion limit of 1000). The parser,
/// `dumps` and `Drop` recurse once per level; without a bound a hostile
/// Cloud JSON answer of nested brackets overflowed the thread stack, which
/// aborts the engine process.
pub const MAX_DEPTH: usize = 900;

impl<'a> Parser<'a> {
    fn err<T>(&self, msg: &str) -> Result<T, JsonError> {
        // json.JSONDecodeError message format: "<msg>: line L column C (char P)"
        let before = &self.text[..self.i.min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let col = match before.rfind('\n') {
            Some(at) => before[at + 1..].chars().count() + 1,
            None => before.chars().count() + 1,
        };
        let pos = before.chars().count();
        Err(JsonError::Decode(format!("{}: line {} column {} (char {})", msg, line, col, pos)))
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Result<Val, JsonError> {
        self.ws();
        if self.i >= self.s.len() {
            return self.err("Expecting value");
        }
        match self.s[self.i] {
            b'{' | b'[' if self.depth >= MAX_DEPTH => Err(JsonError::Value("maximum recursion depth exceeded while decoding a JSON document".into())),
            b'{' => {
                self.depth += 1;
                let v = self.object();
                self.depth -= 1;
                v
            }
            b'[' => {
                self.depth += 1;
                let v = self.array();
                self.depth -= 1;
                v
            }
            b'"' => Ok(Val::Str(self.string()?)),
            b'n' if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Val::Null)
            }
            b't' if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Val::Bool(true))
            }
            b'f' if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Val::Bool(false))
            }
            b'N' if self.s[self.i..].starts_with(b"NaN") => {
                self.i += 3;
                Ok(Val::Float(f64::NAN))
            }
            b'I' if self.s[self.i..].starts_with(b"Infinity") => {
                self.i += 8;
                Ok(Val::Float(f64::INFINITY))
            }
            b'-' if self.s[self.i..].starts_with(b"-Infinity") => {
                self.i += 9;
                Ok(Val::Float(f64::NEG_INFINITY))
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.err("Expecting value"),
        }
    }
    fn number(&mut self) -> Result<Val, JsonError> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let int_start = self.i;
        if self.i < self.s.len() && self.s[self.i] == b'0' {
            self.i += 1;
        } else {
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if self.i == int_start {
            self.i = start;
            return self.err("Expecting value");
        }
        let mut is_float = false;
        if self.i + 1 < self.s.len() && self.s[self.i] == b'.' && self.s[self.i + 1].is_ascii_digit() {
            is_float = true;
            self.i += 1;
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if self.i < self.s.len() && (self.s[self.i] == b'e' || self.s[self.i] == b'E') {
            let save = self.i;
            let mut j = self.i + 1;
            if j < self.s.len() && (self.s[j] == b'+' || self.s[j] == b'-') {
                j += 1;
            }
            let digits = j;
            while j < self.s.len() && self.s[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits {
                is_float = true;
                self.i = j;
            } else {
                self.i = save;
            }
        }
        let text = &self.text[start..self.i];
        if is_float {
            Ok(Val::Float(text.parse::<f64>().unwrap_or(f64::NAN)))
        } else {
            match text.parse::<i64>() {
                Ok(v) => Ok(Val::Int(v)),
                Err(_) => {
                    let t = text.trim_start_matches('-');
                    Ok(Val::BigInt(if text.starts_with('-') { format!("-{}", t) } else { t.to_string() }))
                }
            }
        }
    }
    fn hex4(&mut self) -> Result<u32, JsonError> {
        if self.i + 4 > self.s.len() {
            return self.err("Invalid \\uXXXX escape");
        }
        let h = std::str::from_utf8(&self.s[self.i..self.i + 4]).ok().and_then(|h| {
            if h.bytes().all(|b| b.is_ascii_hexdigit()) {
                u32::from_str_radix(h, 16).ok()
            } else {
                None
            }
        });
        match h {
            Some(v) => {
                self.i += 4;
                Ok(v)
            }
            None => self.err("Invalid \\uXXXX escape"),
        }
    }
    fn string(&mut self) -> Result<String, JsonError> {
        let open = self.i;
        self.i += 1;
        let mut out = String::new();
        loop {
            let run = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\' && self.s[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(&self.text[run..self.i]);
            if self.i >= self.s.len() {
                self.i = open;
                return self.err("Unterminated string starting at");
            }
            match self.s[self.i] {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    if self.i >= self.s.len() {
                        self.i = open;
                        return self.err("Unterminated string starting at");
                    }
                    let c = self.s[self.i];
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.s[self.i..].starts_with(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                                } else {
                                    self.i = save;
                                }
                            }
                            // Lone surrogates cannot live in a Rust String; CPython
                            // keeps them. Map to U+FFFF (rejected by text validation).
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFF}'));
                        }
                        _ => {
                            self.i -= 2;
                            return self.err("Invalid \\escape");
                        }
                    }
                }
                _ => return self.err("Invalid control character at"),
            }
        }
    }
    fn array(&mut self) -> Result<Val, JsonError> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == b']' {
            self.i += 1;
            return Ok(Val::List(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Val::List(items));
                }
                _ => return self.err("Expecting ',' delimiter"),
            }
        }
    }
    fn object(&mut self) -> Result<Val, JsonError> {
        self.i += 1;
        let mut items: Vec<(String, Val)> = Vec::new();
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == b'}' {
            self.i += 1;
            return Ok(Val::Dict(items));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return self.err("Expecting property name enclosed in double quotes");
            }
            let key = self.string()?;
            self.ws();
            if self.s.get(self.i) != Some(&b':') {
                return self.err("Expecting ':' delimiter");
            }
            self.i += 1;
            let value = self.value()?;
            if let Some(slot) = items.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                items.push((key, value));
            }
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Val::Dict(items));
                }
                _ => return self.err("Expecting ',' delimiter"),
            }
        }
    }
}

/// `json.loads(text)`.
pub fn loads(text: &str) -> Result<Val, JsonError> {
    let mut p = Parser { s: text.as_bytes(), text, i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return p.err("Extra data");
    }
    Ok(v)
}
