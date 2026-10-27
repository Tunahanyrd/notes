//! Small pure-Rust helpers that reproduce Python stdlib behaviour exactly.
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// `base64.b64encode(data).decode('ascii')`.
pub fn b64encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn b64value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// `binascii.a2b_base64(data, strict_mode=True)` (`base64.b64decode(..., validate=True)`).
pub fn b64decode_strict(data: &[u8]) -> Option<Vec<u8>> {
    if data.first() == Some(&b'=') {
        return None; // leading padding
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 3);
    let mut quad = 0u8;
    let mut left: u8 = 0;
    let mut pads = 0u8;
    let mut padding = false;
    for (i, &c) in data.iter().enumerate() {
        if c == b'=' {
            padding = true;
            if quad == 0 {
                return None; // excess padding
            }
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    if i + 1 < data.len() {
                        return None; // excess data after padding
                    }
                    return Some(out);
                }
            }
            continue;
        }
        let v = b64value(c)?;
        if padding {
            return None; // discontinuous padding
        }
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push((left << 2) | (v >> 4));
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((left << 4) | (v >> 2));
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push((left << 6) | v);
                left = 0;
            }
        }
    }
    if quad != 0 {
        return None;
    }
    Some(out)
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn hex(data: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("operating system random source unavailable");
    buf
}

/// `secrets.token_hex(n)`.
pub fn token_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("operating system random source unavailable");
    hex(&buf)
}

pub fn format_uuid(b: &[u8; 16]) -> String {
    let h = hex(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// `str(uuid.uuid4())`.
pub fn uuid4() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// Bytes of a canonical lower-case UUID string.
pub fn uuid_bytes(canonical: &str) -> Option<[u8; 16]> {
    if !is_canonical_uuid(canonical) {
        return None;
    }
    let digits: Vec<u8> = canonical.bytes().filter(|&c| c != b'-').collect();
    let mut out = [0u8; 16];
    for i in 0..16 {
        let hi = (digits[2 * i] as char).to_digit(16)? as u8;
        let lo = (digits[2 * i + 1] as char).to_digit(16)? as u8;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// `str(uuid.uuid5(uuid.UUID(namespace), name))` for a canonical namespace.
pub fn uuid5(namespace: &str, name: &str) -> Option<String> {
    use sha1::Sha1;
    let ns = uuid_bytes(namespace)?;
    let mut hasher = Sha1::new();
    hasher.update(ns);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    Some(format_uuid(&b))
}

/// `str(uuid.UUID(value)) == value`.
pub fn is_canonical_uuid(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
        })
}

/// Whether `uuid.UUID(value)` accepts the string (ValueError otherwise).
pub fn uuid_parses(value: &str) -> bool {
    let s = value.replace("urn:", "").replace("uuid:", "");
    let s = s.trim_matches(|c| c == '{' || c == '}');
    let s: String = s.chars().filter(|&c| c != '-').collect();
    if s.chars().count() != 32 {
        return false;
    }
    // int(s, 16): whitespace, sign, 0x prefix, single underscores between digits
    let t = s.trim_matches(crate::mime::py_is_space);
    let (negative, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let t = if t.len() >= 2 && (t.starts_with("0x") || t.starts_with("0X")) {
        let rest = &t[2..];
        rest.strip_prefix('_').unwrap_or(rest)
    } else {
        t
    };
    if t.is_empty() || t.starts_with('_') || t.ends_with('_') || t.contains("__") {
        return false;
    }
    if !t.chars().all(|c| c == '_' || c.is_ascii_hexdigit()) {
        return false;
    }
    // 0 <= int < 2**128
    let digits: String = t.chars().filter(|&c| c != '_').collect();
    let nonzero = digits.trim_start_matches('0');
    if negative && !nonzero.is_empty() {
        return false;
    }
    nonzero.len() <= 32
}

/// `gzip.compress(data, mtime=0)` byte-for-byte (system zlib, level 9).
pub fn gzip_compress(data: &[u8]) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress};
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x02, 0xff]);
    let mut c = Compress::new(Compression::new(9), false);
    let mut buf = Vec::with_capacity(data.len() + data.len() / 1000 + 64);
    loop {
        let consumed = c.total_in() as usize;
        let status = c.compress_vec(&data[consumed..], &mut buf, FlushCompress::Finish).expect("deflate");
        if status == flate2::Status::StreamEnd {
            break;
        }
        buf.reserve(buf.capacity().max(1024));
    }
    out.extend_from_slice(&buf);
    let mut crc = flate2::Crc::new();
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Minimal JSON value for `json.dumps` byte-exact output.
pub enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Str(String),
    List(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn json_str(out: &mut String, s: &str, ensure_ascii: bool) {
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
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(value, indent=indent, ensure_ascii=...)` (indent=None uses ', ' / ': ').
pub fn json_dumps(value: &Json, indent: Option<usize>, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_json(&mut out, value, indent, 0, ensure_ascii);
    out
}

fn write_json(out: &mut String, value: &Json, indent: Option<usize>, level: usize, ascii: bool) {
    let newline = |out: &mut String, level: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * level));
        }
    };
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(i) => out.push_str(&i.to_string()),
        Json::Str(s) => json_str(out, s, ascii),
        Json::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(if indent.is_some() { ',' } else { ',' });
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, level + 1);
                write_json(out, item, indent, level + 1, ascii);
            }
            newline(out, level);
            out.push(']');
        }
        Json::Object(items) => {
            if items.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, v)) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, level + 1);
                json_str(out, k, ascii);
                out.push_str(": ");
                write_json(out, v, indent, level + 1, ascii);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn b64_strict() {
        assert_eq!(b64decode_strict(b"QQ==").unwrap(), b"A");
        assert!(b64decode_strict(b"QQ=").is_none());
        assert!(b64decode_strict(b"QQ==QQ==").is_none());
        assert!(b64decode_strict(b"Q").is_none());
        assert_eq!(b64decode_strict(b"").unwrap(), b"");
        assert_eq!(uuid5("00000000-0000-4000-8000-000000000001", "title").unwrap().len(), 36);
        assert!(uuid_parses("0x000000000000000000000000000001"));
    }
}
