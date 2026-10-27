//! JSON with Python `json` module semantics: byte-identical `json.dumps`
//! (indent=2 / compact / default separators, ensure_ascii, float repr) and the
//! `json.loads` value model (insertion-ordered objects, last duplicate wins at
//! the first position, big integers kept as text).
//!
//! Copied from `crates/rs-cloud/src/pyjson.rs` (pure part only): rs-cloud links
//! PyO3, which must not be reachable from the Python-free engine.
//!
//! Divergences from CPython (see the crate docs): `loads` accepts UTF-8
//! (with optional BOM) only — CPython also auto-detects UTF-16/32 for bytes —
//! and rejects lone surrogate escapes, which CPython keeps in a `str`.
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// Integer outside i64, kept as its decimal text.
    BigInt(String),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// Insertion-ordered like a Python dict.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }
    pub fn obj(items: Vec<(&str, Value)>) -> Value {
        Value::Object(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn s(v: &str) -> Value {
        Value::Str(v.to_string())
    }
}

// ---------------------------------------------------------------- dumps

/// `float.__repr__` for finite values, JSON names for the others.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // Rust's LowerExp without precision is the shortest round-trip form,
    // the same digits as Python's repr (David Gay mode 0).
    let e = format!("{:e}", x.abs());
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1; // position of the decimal point relative to digits
    let sign = if x < 0.0 { "-" } else { "" };
    let n = digits.len() as i32;
    if decpt <= -4 || decpt > 16 {
        let mut s = String::from(sign);
        s.push_str(&digits[..1]);
        if n > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        s.push('e');
        s.push(if e < 0 { '-' } else { '+' });
        s.push_str(&format!("{:02}", e.abs()));
        s
    } else if decpt <= 0 {
        format!("{}0.{}{}", sign, "0".repeat((-decpt) as usize), digits)
    } else if decpt >= n {
        format!("{}{}{}.0", sign, digits, "0".repeat((decpt - n) as usize))
    } else {
        format!("{}{}.{}", sign, &digits[..decpt as usize], &digits[decpt as usize..])
    }
}

/// `json.encoder.py_encode_basestring_ascii` (ensure_ascii=True).
pub fn encode_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
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

pub struct Style<'a> {
    pub indent: Option<usize>,
    pub item_sep: &'a str,
    pub key_sep: &'a str,
}

/// `json.dumps(v, indent=2)`
pub const INDENT2: Style<'static> = Style { indent: Some(2), item_sep: ",", key_sep: ": " };
/// `json.dumps(v, separators=(',', ':'))`
pub const COMPACT: Style<'static> = Style { indent: None, item_sep: ",", key_sep: ":" };
/// `json.dumps(v)`
pub const DEFAULT: Style<'static> = Style { indent: None, item_sep: ", ", key_sep: ": " };

pub fn dumps(v: &Value, style: &Style) -> String {
    let mut out = String::new();
    write_value(&mut out, v, style, 0);
    out
}

fn newline(out: &mut String, style: &Style, level: usize) {
    if let Some(n) = style.indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

fn write_value(out: &mut String, v: &Value, style: &Style, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::BigInt(s) => out.push_str(s),
        Value::Float(f) => out.push_str(&float_repr(*f)),
        Value::Str(s) => encode_str(out, s),
        Value::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(style.item_sep);
                }
                newline(out, style, level + 1);
                write_value(out, item, style, level + 1);
            }
            newline(out, style, level);
            out.push(']');
        }
        Value::Object(items) => {
            if items.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(style.item_sep);
                }
                newline(out, style, level + 1);
                encode_str(out, k);
                out.push_str(style.key_sep);
                write_value(out, item, style, level + 1);
            }
            newline(out, style, level);
            out.push('}');
        }
    }
}

// ---------------------------------------------------------------- loads

#[derive(Debug)]
pub struct DecodeError {
    pub msg: String,
    pub pos: usize,
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

fn err<T>(msg: &str, pos: usize) -> Result<T, DecodeError> {
    Err(DecodeError { msg: msg.into(), pos })
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn char_pos(&self, byte: usize) -> usize {
        self.text[..byte.min(self.text.len())].chars().count()
    }
    fn fail<T>(&self, msg: &str, byte: usize) -> Result<T, DecodeError> {
        err(msg, self.char_pos(byte))
    }
    fn starts(&self, lit: &str) -> bool {
        self.s[self.i..].starts_with(lit.as_bytes())
    }
    fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > 900 {
            return self.fail("maximum recursion depth exceeded", self.i);
        }
        if self.i >= self.s.len() {
            return self.fail("Expecting value", self.i);
        }
        match self.s[self.i] {
            b'"' => {
                self.i += 1;
                Ok(Value::Str(self.string()?))
            }
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'n' if self.starts("null") => {
                self.i += 4;
                Ok(Value::Null)
            }
            b't' if self.starts("true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            b'f' if self.starts("false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            b'N' if self.starts("NaN") => {
                self.i += 3;
                Ok(Value::Float(f64::NAN))
            }
            b'I' if self.starts("Infinity") => {
                self.i += 8;
                Ok(Value::Float(f64::INFINITY))
            }
            b'-' if self.starts("-Infinity") => {
                self.i += 9;
                Ok(Value::Float(f64::NEG_INFINITY))
            }
            _ => self.number(),
        }
    }
    fn number(&mut self) -> Result<Value, DecodeError> {
        let start = self.i;
        let mut j = self.i;
        let s = self.s;
        if j < s.len() && s[j] == b'-' {
            j += 1;
        }
        if j < s.len() && s[j] == b'0' {
            j += 1;
        } else if j < s.len() && (b'1'..=b'9').contains(&s[j]) {
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
        } else {
            return self.fail("Expecting value", start);
        }
        let int_end = j;
        let mut is_float = false;
        if j + 1 < s.len() && s[j] == b'.' && s[j + 1].is_ascii_digit() {
            j += 1;
            while j < s.len() && s[j].is_ascii_digit() {
                j += 1;
            }
            is_float = true;
        }
        if j < s.len() && (s[j] == b'e' || s[j] == b'E') {
            let mut k = j + 1;
            if k < s.len() && (s[k] == b'+' || s[k] == b'-') {
                k += 1;
            }
            if k < s.len() && s[k].is_ascii_digit() {
                while k < s.len() && s[k].is_ascii_digit() {
                    k += 1;
                }
                j = k;
                is_float = true;
            }
        }
        self.i = j;
        let text = std::str::from_utf8(&s[start..j]).unwrap();
        if is_float {
            Ok(Value::Float(text.parse::<f64>().unwrap_or(f64::NAN)))
        } else {
            let text = &self.text[start..int_end];
            match text.parse::<i64>() {
                Ok(v) => Ok(Value::Int(v)),
                Err(_) => Ok(Value::BigInt(text.to_string())),
            }
        }
    }
    fn hex4(&self, at: usize) -> Result<u32, DecodeError> {
        if at + 4 > self.s.len() {
            return self.fail("Invalid \\uXXXX escape", at - 1);
        }
        let h = std::str::from_utf8(&self.s[at..at + 4]).map_err(|_| DecodeError { msg: "Invalid \\uXXXX escape".into(), pos: self.char_pos(at - 1) })?;
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return self.fail("Invalid \\uXXXX escape", at - 1);
        }
        Ok(u32::from_str_radix(h, 16).unwrap())
    }
    fn string(&mut self) -> Result<String, DecodeError> {
        let begin = self.i - 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\' && self.s[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(&self.text[start..self.i]);
            if self.i >= self.s.len() {
                return self.fail("Unterminated string starting at", begin);
            }
            let c = self.s[self.i];
            if c == b'"' {
                self.i += 1;
                return Ok(out);
            }
            if c < 0x20 {
                return self.fail("Invalid control character at", self.i);
            }
            // backslash
            self.i += 1;
            if self.i >= self.s.len() {
                return self.fail("Unterminated string starting at", begin);
            }
            let e = self.s[self.i];
            self.i += 1;
            match e {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{08}'),
                b'f' => out.push('\u{0c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let mut code = self.hex4(self.i)?;
                    self.i += 4;
                    if (0xD800..0xDC00).contains(&code) && self.starts("\\u") {
                        let low = self.hex4(self.i + 2)?;
                        if (0xDC00..0xE000).contains(&low) {
                            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            self.i += 6;
                        }
                    }
                    match char::from_u32(code) {
                        Some(ch) => out.push(ch),
                        // A lone surrogate is a valid Python str but not a Rust
                        // String; this receipt format never writes one.
                        None => return self.fail("Unsupported lone surrogate escape", self.i - 6),
                    }
                }
                _ => return self.fail("Invalid \\escape", self.i - 2),
            }
        }
    }
    fn array(&mut self, depth: usize) -> Result<Value, DecodeError> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == b']' {
            self.i += 1;
            return Ok(Value::List(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b']' {
                self.i += 1;
                return Ok(Value::List(items));
            }
            if self.i < self.s.len() && self.s[self.i] == b',' {
                self.i += 1;
                self.ws();
                if self.i < self.s.len() && self.s[self.i] == b']' {
                    return self.fail("Illegal trailing comma before end of array", self.i - 1);
                }
                continue;
            }
            return self.fail("Expecting ',' delimiter", self.i);
        }
    }
    fn object(&mut self, depth: usize) -> Result<Value, DecodeError> {
        self.i += 1;
        let mut items: Vec<(String, Value)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == b'}' {
            self.i += 1;
            return Ok(Value::Object(items));
        }
        loop {
            self.ws();
            if self.i >= self.s.len() || self.s[self.i] != b'"' {
                return self.fail("Expecting property name enclosed in double quotes", self.i);
            }
            self.i += 1;
            let key = self.string()?;
            self.ws();
            if self.i >= self.s.len() || self.s[self.i] != b':' {
                return self.fail("Expecting ':' delimiter", self.i);
            }
            self.i += 1;
            self.ws();
            let value = self.value(depth + 1)?;
            match index.get(&key) {
                Some(&at) => items[at].1 = value,
                None => {
                    index.insert(key.clone(), items.len());
                    items.push((key, value));
                }
            }
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b'}' {
                self.i += 1;
                return Ok(Value::Object(items));
            }
            if self.i < self.s.len() && self.s[self.i] == b',' {
                self.i += 1;
                self.ws();
                if self.i < self.s.len() && self.s[self.i] == b'}' {
                    return self.fail("Illegal trailing comma before end of object", self.i - 1);
                }
                continue;
            }
            return self.fail("Expecting ',' delimiter", self.i);
        }
    }
}

/// `json.loads(bytes)` for UTF-8 input (a UTF-8 BOM is accepted for bytes).
pub fn loads(raw: &[u8]) -> Result<Value, DecodeError> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(raw);
    let text = match std::str::from_utf8(raw) {
        Ok(t) => t,
        Err(e) => return err("Invalid UTF-8 input", e.valid_up_to()),
    };
    let mut p = Parser { s: text.as_bytes(), text, i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return p.fail("Extra data", p.i);
    }
    Ok(v)
}


// ---------------------------------------------------------------- Python semantics

impl Value {
    /// `isinstance(value, dict)`
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// `type(value).__name__`
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::BigInt(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Object(_) => "dict",
        }
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::BigInt(_) => true,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }

    /// `type(value) is int` (bool is excluded).
    pub fn is_strict_int(&self) -> bool {
        matches!(self, Value::Int(_) | Value::BigInt(_))
    }

    /// `type(value) in (int, float)` as a float (bool excluded). Big integers
    /// are approximated; they can never pass the bounded checks that use this.
    pub fn strict_number(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::BigInt(s) => s.parse::<f64>().ok(),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Any Python number (bool included) for `<`/`+` arithmetic; other types
    /// raise `TypeError` in Python.
    pub fn any_number(&self) -> Option<f64> {
        match self {
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            other => other.strict_number(),
        }
    }

    /// `dict.insert`/`d[key] = value` keeping the first position.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Object(items) = self {
            if let Some(slot) = items.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value;
            } else {
                items.push((key.to_string(), value));
            }
        }
    }
}

fn num_eq(a: &Value, b: &Value) -> Option<bool> {
    let n = |v: &Value| -> Option<Num> {
        match v {
            Value::Bool(x) => Some(Num::I(*x as i64)),
            Value::Int(x) => Some(Num::I(*x)),
            Value::BigInt(s) => Some(Num::B(s.clone())),
            Value::Float(f) => Some(Num::F(*f)),
            _ => None,
        }
    };
    let (x, y) = (n(a)?, n(b)?);
    Some(match (x, y) {
        (Num::I(p), Num::I(q)) => p == q,
        (Num::I(p), Num::F(q)) | (Num::F(q), Num::I(p)) => (p as f64) == q && q.fract() == 0.0 && (q as i64) == p,
        (Num::F(p), Num::F(q)) => p == q,
        (Num::B(p), Num::B(q)) => p == q,
        (Num::B(p), Num::F(q)) | (Num::F(q), Num::B(p)) => p.parse::<f64>().map(|v| v == q && q.fract() == 0.0).unwrap_or(false) && format!("{:.0}", q) == p,
        _ => false,
    })
}

enum Num {
    I(i64),
    B(String),
    F(f64),
}

/// Python `a == b` for JSON values (dicts compare without order, numbers
/// compare across int/float/bool).
pub fn py_eq(a: &Value, b: &Value) -> bool {
    if let Some(result) = num_eq(a, b) {
        return result;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && py_eq(v, v2)))
        }
        _ => false,
    }
}

/// `value == n` for an optional value (`dict.get` returning None when absent).
pub fn eq_int(value: Option<&Value>, n: i64) -> bool {
    value.is_some_and(|v| py_eq(v, &Value::Int(n)))
}

/// `value == text` for an optional value.
pub fn eq_str(value: Option<&Value>, text: &str) -> bool {
    matches!(value, Some(Value::Str(s)) if s == text)
}

/// `json.dumps(value, indent=2) + '\n'` as bytes (notes_login.save_json).
pub fn indent2_bytes(value: &Value) -> Vec<u8> {
    let mut s = dumps(value, &INDENT2);
    s.push('\n');
    s.into_bytes()
}
