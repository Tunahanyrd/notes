//! Pure-Rust helpers with the exact semantics of the Python helpers they
//! replace (tools/notes_login.py private_write/load_json/save_json,
//! cloud_api._save_once/_json_once/_uuid/_string, gzip.compress(mtime=0),
//! urllib.parse quoting, uuid4/uuid5, secrets).
use crate::pyjson::{self, Value};
use sha1::Digest as _;
use sha2::Digest as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

// ------------------------------------------------------------ files

/// notes_login.private_write: O_CREAT|O_EXCL|O_WRONLY 0600, fsync.
pub fn private_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()
}

pub enum LoadError {
    Io(io::Error),
    TooLarge,
    Json(pyjson::DecodeError, Vec<u8>),
}

/// notes_login.load_json(path, limit)
pub fn load_json(path: &Path, limit: usize) -> Result<Value, LoadError> {
    let f = fs::File::open(path).map_err(LoadError::Io)?;
    let mut raw = Vec::new();
    f.take(limit as u64 + 1).read_to_end(&mut raw).map_err(LoadError::Io)?;
    if raw.len() > limit {
        return Err(LoadError::TooLarge);
    }
    pyjson::loads(&raw).map_err(|e| LoadError::Json(e, raw))
}

pub fn json_bytes(value: &Value) -> Vec<u8> {
    let mut s = pyjson::dumps(value, &pyjson::INDENT2);
    s.push('\n');
    s.into_bytes()
}

/// notes_login.save_json
pub fn save_json(path: &Path, value: &Value) -> io::Result<()> {
    private_write(path, &json_bytes(value))
}

pub enum OnceError {
    Io(io::Error),
    Mismatch,
}

/// cloud_api._save_once
pub fn save_once(path: &Path, data: &[u8]) -> Result<(), OnceError> {
    if path.exists() {
        if fs::read(path).map_err(OnceError::Io)? != data {
            return Err(OnceError::Mismatch);
        }
        Ok(())
    } else {
        private_write(path, data).map_err(OnceError::Io)
    }
}

/// `Path.mkdir(mode=0o700, exist_ok=...)` (mode masked by umask like Python).
pub fn mkdir_private(path: &Path, exist_ok: bool) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if exist_ok && e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

// ------------------------------------------------------------ gzip

/// `gzip.compress(data, mtime=0)` on CPython 3.12+: zlib level 9, wbits 31,
/// header mtime 0 and OS byte 255. Uses the system zlib like CPython.
pub fn gzip_mtime0(data: &[u8]) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress, Status};
    let mut c = Compress::new_gzip(Compression::new(9), 15);
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    loop {
        if out.capacity() - out.len() < 1024 {
            out.reserve(out.capacity().max(4096));
        }
        let consumed = c.total_in() as usize;
        match c.compress_vec(&data[consumed..], &mut out, FlushCompress::Finish) {
            Ok(Status::StreamEnd) => break,
            Ok(_) => continue,
            Err(e) => panic!("deflate failed: {e}"),
        }
    }
    out[4..8].copy_from_slice(&[0, 0, 0, 0]);
    out[9] = 255;
    out
}

pub enum GunzipError {
    Io(io::Error),
}

/// `gzip.GzipFile(fileobj=BytesIO(data)).read(limit)`
pub fn gunzip_limited(data: &[u8], limit: usize) -> Result<Vec<u8>, GunzipError> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(data)
        .take(limit as u64)
        .read_to_end(&mut out)
        .map_err(GunzipError::Io)?;
    Ok(out)
}

// ------------------------------------------------------------ hashes / ids

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha2::Sha256::digest(data))
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(data).into()
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("OS randomness unavailable");
    buf
}

fn format_uuid(b: &[u8; 16]) -> String {
    let h = hex(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// `str(uuid.uuid4())`
pub fn uuid4() -> String {
    let mut b: [u8; 16] = random_bytes(16).try_into().unwrap();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// `str(uuid.uuid5(uuid.UUID(namespace), name))` for a canonical namespace.
pub fn uuid5(namespace: &str, name: &str) -> String {
    let ns: Vec<u8> = (0..16)
        .map(|i| {
            let h: String = namespace.chars().filter(|c| *c != '-').collect();
            u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()
        })
        .collect();
    let mut hasher = sha1::Sha1::new();
    hasher.update(&ns);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut b: [u8; 16] = digest[..16].try_into().unwrap();
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// `str(uuid.UUID(value)) == value`: canonical lowercase 8-4-4-4-12 form.
pub fn is_canonical_uuid(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => matches!(c, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `secrets.token_hex(n)`
pub fn token_hex(n: usize) -> String {
    hex(&random_bytes(n))
}

/// `''.join(secrets.choice(alphabet) for _ in range(n))`, uniform.
pub fn random_choice(alphabet: &str, n: usize) -> String {
    let chars: Vec<char> = alphabet.chars().collect();
    let m = chars.len() as u32;
    let zone = u32::MAX - (u32::MAX % m);
    let mut out = String::new();
    while out.chars().count() < n {
        let r = u32::from_le_bytes(random_bytes(4).try_into().unwrap());
        if r < zone {
            out.push(chars[(r % m) as usize]);
        }
    }
    out
}

// ------------------------------------------------------------ strings

/// cloud_api._string for an already-extracted str.
pub fn is_safe_string(value: &str, limit: usize) -> bool {
    let n = value.chars().count();
    0 < n && n <= limit && !value.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
}

/// Python `str.isspace()` for one character.
pub fn py_isspace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '\u{1c}'..='\u{1f}' | ' ' | '\u{85}' | '\u{a0}')
        || (c as u32 > 0xff && c.is_whitespace())
}

/// Python `str.strip()`
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python `int(text)` restricted to ASCII digits (whitespace, sign and single
/// underscores between digits accepted like CPython). Non-ASCII digits are
/// rejected, which CPython would accept; Samsung metadata is ASCII.
pub fn py_int(text: &str) -> Option<i128> {
    let t = py_strip(text);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if !clean.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = clean.parse().ok()?;
    Some(if neg { -v } else { v })
}

// ------------------------------------------------------------ URLs

fn quote_into(out: &mut String, s: &str, plus: bool) {
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(*b as char),
            b' ' if plus => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
}

/// `urllib.parse.quote(s, safe='')`
pub fn quote(s: &str) -> String {
    let mut out = String::new();
    quote_into(&mut out, s, false);
    out
}

/// `urllib.parse.urlencode(pairs)` for str values.
pub fn urlencode(pairs: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        quote_into(&mut out, k, true);
        out.push('=');
        quote_into(&mut out, v, true);
    }
    out
}

pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
}

impl SplitUrl {
    /// `urlsplit(url).hostname`
    pub fn hostname(&self) -> Option<String> {
        let hostinfo = self.netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(&self.netloc);
        let host = if let Some(rest) = hostinfo.strip_prefix('[') {
            rest.split_once(']').map(|(h, _)| h).unwrap_or("")
        } else {
            hostinfo.split_once(':').map(|(h, _)| h).unwrap_or(hostinfo)
        };
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    }
    /// (`username`, `password`) like urlsplit.
    pub fn userinfo(&self) -> (Option<String>, Option<String>) {
        match self.netloc.rsplit_once('@') {
            None => (None, None),
            Some((user, _)) => match user.split_once(':') {
                Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
                None => (Some(user.to_string()), None),
            },
        }
    }
}

/// The subset of `urllib.parse.urlsplit` the upload URL check uses.
/// Err(()) where urlsplit raises ValueError ("Invalid IPv6 URL").
pub fn urlsplit(url: &str) -> Result<SplitUrl, ()> {
    let trimmed = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let cleaned: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest = cleaned.as_str();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let cand = &rest[..i];
        if i > 0
            && cand.as_bytes()[0].is_ascii_alphabetic()
            && cand.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        {
            scheme = cand.to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        if netloc.contains('[') != netloc.contains(']') {
            return Err(());
        }
    }
    Ok(SplitUrl { scheme, netloc })
}

/// notes_cloud.endpoint semantics. Err(message) -> ValueError(message).
pub fn endpoint(value: Option<&Value>) -> Result<String, &'static str> {
    let value = match value {
        Some(Value::Str(s)) => s.as_str(),
        _ => return Err("missing Cloud endpoint"),
    };
    let trimmed = value.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let cleaned: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let ok = (|| {
        let parts = urlsplit(value).ok()?;
        let after = cleaned.split_once("://")?.1;
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let tail = &after[end..];
        let (path_query, fragment) = match tail.split_once('#') {
            Some((a, b)) => (a, Some(b)),
            None => (tail, None),
        };
        let (path, query) = match path_query.split_once('?') {
            Some((a, b)) => (a, b),
            None => (path_query, ""),
        };
        let host = parts.hostname();
        let allowed = matches!(host.as_deref(), Some("api.samsungcloud.com") | Some("play.samsungcloud.com"));
        Some(
            parts.scheme == "https"
                && allowed
                && Some(parts.netloc.clone()) == host
                && (path.is_empty() || path == "/")
                && query.is_empty()
                && fragment.map(|f| f.is_empty()).unwrap_or(true),
        )
    })()
    .unwrap_or(false);
    if !ok {
        return Err("Cloud endpoint outside observed allowlist");
    }
    Ok(value.trim_end_matches('/').to_string())
}
