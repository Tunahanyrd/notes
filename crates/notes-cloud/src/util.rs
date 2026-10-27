//! Helpers with the exact semantics of the Python helpers they replace
//! (tools/notes_login.py private_write/load_json/save_json,
//! cloud_api._save_once/_json_once/_uuid/_string, gzip.compress(mtime=0),
//! urllib.parse quoting, uuid4/uuid5, secrets, notes_cloud.endpoint).
use crate::json;
use notes_host::{Fail, Kind, Val};
use sha1::Digest as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

pub type R<T> = Result<T, Fail>;

/// `notes_login.load_json` default limit.
pub const DEFAULT_LIMIT: usize = 128 * 1024;

// ------------------------------------------------------------ files

/// notes_login.private_write: O_CREAT|O_EXCL|O_WRONLY 0600, fsync.
pub fn private_write(path: &Path, data: &[u8]) -> R<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

/// notes_login.load_json(path, limit)
pub fn load_json(path: &Path, limit: usize) -> R<Val> {
    let f = fs::File::open(path)?;
    let mut raw = Vec::new();
    f.take(limit as u64 + 1).read_to_end(&mut raw)?;
    if raw.len() > limit {
        return Err(Fail::value("private input exceeds size limit"));
    }
    json::loads(&raw)
}

/// notes_login.save_json
pub fn save_json(path: &Path, value: &Val) -> R<()> {
    private_write(path, &json::receipt_bytes(value))
}

/// cloud_api._save_once
pub fn save_once(path: &Path, data: &[u8]) -> R<()> {
    if path.exists() {
        if fs::read(path)? != data {
            return Err(Fail::api("operation_mismatch", "Hazırlanmış işlemin içeriği değişti."));
        }
        Ok(())
    } else {
        private_write(path, data)
    }
}

/// cloud_api._json_once
pub fn json_once(path: &Path, value: &Val) -> R<()> {
    save_once(path, &json::receipt_bytes(value))
}

/// `Path.mkdir(mode=0o700, exist_ok=...)` (mode masked by umask like Python).
pub fn mkdir_private(path: &Path, exist_ok: bool) -> R<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if exist_ok && e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(Fail::new(Kind::Os, format!("[Errno 17] File exists: '{}'", path.display()))),
        other => other.map_err(Fail::from),
    }
}

/// `path.read_bytes()`
pub fn read_bytes(path: &Path) -> R<Vec<u8>> {
    Ok(fs::read(path)?)
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

/// `gzip.GzipFile(fileobj=BytesIO(data)).read(limit)` (errors: OSError/EOFError).
pub fn gunzip_limited(data: &[u8], limit: usize) -> R<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(data).take(limit as u64).read_to_end(&mut out)?;
    Ok(out)
}

// ------------------------------------------------------------ hashes / ids

pub fn sha256(data: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(data).into()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub fn b64encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Source of the random values Python draws from `uuid.uuid4()` and
/// `secrets` (injectable so receipts can be compared byte for byte).
pub trait Random: Send + Sync {
    /// `str(uuid.uuid4())`
    fn uuid4(&self) -> String;
    /// `secrets.token_hex(n)`
    fn token_hex(&self, n: usize) -> String;
    /// `''.join(secrets.choice(alphabet) for _ in range(n))`
    fn choice(&self, alphabet: &str, n: usize) -> String;
    /// `secrets.token_urlsafe(n)`
    fn token_urlsafe(&self, n: usize) -> String;
}

/// OS randomness (the default).
pub struct OsRandom;

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("OS randomness unavailable");
    buf
}

fn format_uuid(b: &[u8; 16]) -> String {
    let h = hex(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

impl Random for OsRandom {
    fn token_urlsafe(&self, n: usize) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(n))
    }
    fn uuid4(&self) -> String {
        let mut b: [u8; 16] = random_bytes(16).try_into().unwrap();
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        format_uuid(&b)
    }
    fn token_hex(&self, n: usize) -> String {
        hex(&random_bytes(n))
    }
    fn choice(&self, alphabet: &str, n: usize) -> String {
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
}

/// `str(uuid.uuid5(uuid.UUID(namespace), name))` for a canonical namespace.
pub fn uuid5(namespace: &str, name: &str) -> String {
    let h: String = namespace.chars().filter(|c| *c != '-').collect();
    let ns: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect();
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

/// cloud_api._uuid
pub fn check_uuid(value: &str) -> R<String> {
    if is_canonical_uuid(value) {
        Ok(value.to_string())
    } else {
        Err(Fail::api("invalid_operation", "İşlem kimliği geçersiz."))
    }
}

/// cloud_api._uuid for an arbitrary value (non-str fails the same way).
pub fn check_uuid_val(value: &Val) -> R<String> {
    match value {
        Val::Str(s) => check_uuid(s),
        _ => Err(Fail::api("invalid_operation", "İşlem kimliği geçersiz.")),
    }
}

// ------------------------------------------------------------ strings

/// cloud_api._string for an already-extracted str.
pub fn is_safe_string(value: &str, limit: usize) -> bool {
    let n = value.chars().count();
    0 < n && n <= limit && !value.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
}

/// cloud_api._string(value, limit)
pub fn string_ok(value: Option<&Val>, limit: usize) -> bool {
    matches!(value, Some(Val::Str(s)) if is_safe_string(s, limit))
}

/// exchange_account_code.safe_text(value, maximum)
pub fn safe_text(value: Option<&Val>, maximum: usize) -> bool {
    match value {
        Some(Val::Str(s)) => {
            let n = s.chars().count();
            0 < n && n <= maximum && s.chars().all(|c| 32 < c as u32 && (c as u32) < 127)
        }
        _ => false,
    }
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

/// Python `int(text)` restricted to ASCII digits.
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

/// Python `str.capitalize()` (urllib header-name normalisation).
pub fn capitalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        // CPython uses the titlecase mapping for the first character.
        out.extend(first.to_uppercase());
        for c in chars {
            out.extend(c.to_lowercase());
        }
    }
    out
}

// ------------------------------------------------------------ numbers

/// `type(v) in (int, float)` (bool excluded).
pub fn is_num(v: Option<&Val>) -> bool {
    matches!(v, Some(Val::Int(_) | Val::BigInt(_) | Val::Float(_)))
}

pub fn num(v: &Val) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

/// `a + b` for two JSON numbers (int+int stays int).
pub fn num_add(a: &Val, b: &Val) -> Val {
    match (a, b) {
        (Val::Int(x), Val::Int(y)) => match x.checked_add(*y) {
            Some(v) => Val::Int(v),
            None => Val::BigInt(((*x as i128) + (*y as i128)).to_string()),
        },
        _ => Val::Float(num(a) + num(b)),
    }
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

/// The subset of `urllib.parse.urlsplit` the URL checks use.
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

/// notes_cloud.endpoint(endpoints, role). Errors are ValueError.
pub fn endpoint(endpoints: &Val, role: &str) -> R<String> {
    let value = match endpoints {
        Val::Dict(_) => match endpoints.get(role) {
            Some(Val::Str(s)) => s.as_str(),
            _ => return Err(Fail::value("missing Cloud endpoint")),
        },
        other => return Err(Fail::new(Kind::Attribute, format!("'{}' object has no attribute 'get'", other.type_name()))),
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
        return Err(Fail::value("Cloud endpoint outside observed allowlist"));
    }
    Ok(value.trim_end_matches('/').to_string())
}

/// `dict.get(key)` on a value that must be a dict (AttributeError otherwise).
pub fn dget<'a>(v: &'a Val, key: &str) -> R<Option<&'a Val>> {
    match v {
        Val::Dict(_) => Ok(v.get(key)),
        other => Err(Fail::new(Kind::Attribute, format!("'{}' object has no attribute 'get'", other.type_name()))),
    }
}

/// `value[key]` (KeyError / TypeError like Python).
pub fn ditem<'a>(v: &'a Val, key: &str) -> R<&'a Val> {
    match v {
        Val::Dict(_) => v.get(key).ok_or_else(|| Fail::key(format!("'{}'", key))),
        Val::Null => Err(Fail::type_("'NoneType' object is not subscriptable")),
        other => Err(Fail::type_(format!("{} indices must be integers", other.type_name()))),
    }
}

/// `value.get(key, {})` chained: `d.get(a, {}).get(b)`.
pub fn dget2<'a>(v: &'a Val, a: &str, b: &str) -> R<Option<&'a Val>> {
    match dget(v, a)? {
        None => Ok(None),
        Some(inner) => dget(inner, b),
    }
}
