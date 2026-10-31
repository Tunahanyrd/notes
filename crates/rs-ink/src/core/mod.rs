//! Pure-Rust ink core (no PyO3). The `#[pyfunction]` wrappers in the sibling
//! modules only convert Python objects and map `InkErr` to Python exceptions.
pub mod binary;
pub mod cache;
pub mod capture;
pub mod coords;
pub mod num;
pub mod objects;
pub mod payload;
pub mod prepare;
pub mod records;
pub mod xml;

/// Error kinds mirror the Python exception classes raised by the oracle.
#[derive(Debug, Clone)]
pub enum InkErr {
    /// samsung_notes.ink.InkError
    Ink(String),
    /// samsung_notes.document.DocumentError (Turkish message verbatim)
    Document(String),
    /// tools.notes_payload.PayloadError
    Payload(String),
    /// struct.error
    Struct(String),
    KeyError(String),
    ValueError(String),
    TypeError(String),
    Overflow(String),
    /// xml.etree.ElementTree.ParseError
    Parse(String),
    /// UnicodeDecodeError(encoding, bytes, start, end, reason)
    Unicode(&'static str, Vec<u8>, usize, usize, &'static str),
    /// An error raised by an injected callback (e.g. a Python fallback);
    /// the caller keeps the original error object.
    Foreign,
}

pub type R<T> = Result<T, InkErr>;

impl From<rs_doc::Error> for InkErr {
    fn from(e: rs_doc::Error) -> Self {
        match e {
            rs_doc::Error::Payload(m) => InkErr::Payload(m.to_string()),
            rs_doc::Error::Document(m) => InkErr::Document(m.into_owned()),
            rs_doc::Error::Value(m) => InkErr::ValueError(m.into_owned()),
            rs_doc::Error::Type(m) => InkErr::TypeError(m.into_owned()),
            rs_doc::Error::XmlParse(m) => InkErr::Parse(m),
            rs_doc::Error::Io(e) => InkErr::ValueError(e.to_string()),
            rs_doc::Error::External(_) => InkErr::Foreign,
        }
    }
}

pub fn ink<T>(msg: &str) -> R<T> {
    Err(InkErr::Ink(msg.to_string()))
}

pub const DOC_STRUCTURE: &str = "Belgenin yapısı güvenli düzenleme için uygun değil.";
pub const DOC_INTEGER: &str = "Belge zaman veya sürüm bilgisi geçersiz.";

/// samsung_notes.document.integer
pub fn doc_integer(value: Option<&str>) -> R<i64> {
    let v = match value {
        Some(v) => v,
        None => return Err(InkErr::Document(DOC_INTEGER.into())),
    };
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(InkErr::Document(DOC_INTEGER.into()));
    }
    let n: u64 = v.parse().map_err(|_| InkErr::Document(DOC_INTEGER.into()))?;
    if n > i64::MAX as u64 {
        return Err(InkErr::Document(DOC_INTEGER.into()));
    }
    Ok(n as i64)
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(data))
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(data).into()
}

pub fn hex(data: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// `str(uuid.UUID(v)) == v`: canonical lower-case hyphenated form.
pub fn is_canonical_uuid(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(c)
            }
        })
}

/// Whether `uuid.UUID(v)` would parse (any accepted spelling).
pub fn uuid_parses(v: &str) -> bool {
    let mut s = v.replace("urn:", "").replace("uuid:", "");
    s = s.trim_matches(|c| c == '{' || c == '}').replace('-', "");
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// tools.notes_writer._uuid
pub fn writer_uuid(v: Option<&str>) -> R<()> {
    match v {
        None => Err(InkErr::ValueError("canonical lower-case UUID required".into())),
        Some(s) if !uuid_parses(s) => Err(InkErr::ValueError("badly formed hexadecimal UUID string".into())),
        Some(s) if !is_canonical_uuid(s) => Err(InkErr::ValueError("canonical lower-case UUID required".into())),
        _ => Ok(()),
    }
}

/// tools.notes_writer.object_hash
pub fn object_hash(id: Option<&str>, ms: i128) -> R<[u8; 32]> {
    writer_uuid(id)?;
    if !(1..=i64::MAX as i128).contains(&ms) {
        return Err(InkErr::ValueError("positive signed-64-bit millisecond timestamp required".into()));
    }
    Ok(sha256(format!("{}{}", id.unwrap(), ms).as_bytes()))
}

/// tools.notes_writer.parent_hash
pub fn parent_hash(id: Option<&str>, ms: i128, children: &[Vec<u8>]) -> R<[u8; 32]> {
    if children.iter().any(|h| h.len() != 32) {
        return Err(InkErr::ValueError("child hashes must be raw SHA256 bytes".into()));
    }
    let own = object_hash(id, ms)?;
    let mut all = Vec::with_capacity(children.len() * 32 + 32);
    for c in children {
        all.extend_from_slice(c);
    }
    all.extend_from_slice(&own);
    Ok(sha256(&all))
}

/// bytes.fromhex(value)
pub fn fromhex(value: Option<&str>) -> R<Vec<u8>> {
    let s = match value {
        Some(s) => s,
        None => return Err(InkErr::TypeError("fromhex() argument must be str, not None".into())),
    };
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let bad = |i: usize| InkErr::ValueError(format!("non-hexadecimal number found in fromhex() arg at position {i}"));
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let hi = c.to_digit(16).filter(|_| c.is_ascii()).ok_or_else(|| bad(i))?;
        if i + 1 >= chars.len() {
            return Err(bad(i + 1));
        }
        let c2 = chars[i + 1];
        let lo = c2.to_digit(16).filter(|_| c2.is_ascii()).ok_or_else(|| bad(i + 1))?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Ok(out)
}

/// base64.b64decode(value, validate=True) on a str.
pub fn b64decode_strict(value: &str) -> Option<Vec<u8>> {
    use base64::engine::{general_purpose::GeneralPurpose, general_purpose::GeneralPurposeConfig, DecodePaddingMode};
    use base64::Engine;
    if !value.is_ascii() {
        return None;
    }
    let engine = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    );
    engine.decode(value).ok()
}

/// Python `int(text)` for str input (ASCII digits subset).
pub fn py_int(text: &str) -> Option<i128> {
    let t = text.trim();
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) || digits.len() > 36 {
        return None;
    }
    let v: i128 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Python `str.lower()` (Unicode-aware; ASCII result for the compared values).
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}
