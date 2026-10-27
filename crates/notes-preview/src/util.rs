//! Small shared helpers: error constructors, private atomic writes, bounded
//! cache reads, hex digests and Python-compatible JSON emission.
use notes_host::{Fail, Kind};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const PREVIEW_DEFAULT: &str = "Bu sayfanın görüntüsü henüz açılamıyor.";
pub const PDF_DEFAULT: &str = "PDF sayfası güvenle görüntülenemedi.";

/// `PreviewError(message)`
pub fn preview(message: &str) -> Fail {
    Fail::new(Kind::Preview, message)
}

/// `PdfBackgroundError(message)`
pub fn pdf(message: &str) -> Fail {
    Fail::new(Kind::PdfBackground, message)
}

/// `InkRenderError(message)`
pub fn ink_render(message: &str) -> Fail {
    Fail::new(Kind::InkRender, message)
}

/// `rs_store` errors (private_directory, meta, queries) as `Fail`.
pub fn store_fail(e: rs_store::db::Error) -> Fail {
    Fail::from(e)
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

pub fn hex(data: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(data.len() * 2);
    for b in data {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

/// `secrets.token_hex(n)`
pub fn token_hex(n: usize) -> String {
    let mut buffer = vec![0u8; n];
    if getrandom::fill(&mut buffer).is_err() {
        // Fallback: time and address entropy; only used for temporary names
        // that are additionally opened with O_EXCL.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let seed = sha256(format!("{nanos}:{:p}:{}", &buffer, std::process::id()).as_bytes());
        for (i, b) in buffer.iter_mut().enumerate() {
            *b = seed[i % 32];
        }
    }
    hex(&buffer)
}

/// `path.with_suffix('.partial-' + token_hex(n))`
pub fn partial_path(path: &Path, n: usize) -> PathBuf {
    path.with_extension(format!("partial-{}", token_hex(n)))
}

/// `storage.private_directory(path)`
pub fn private_directory(path: &Path) -> Result<PathBuf, Fail> {
    rs_store::storage::private_directory(path).map_err(store_fail)
}

/// `path.is_file() and not path.is_symlink() and path.stat().st_size <= limit`,
/// then `path.read_bytes()`. `Ok(None)` when the gate fails; read errors
/// propagate like Python's `read_bytes`.
pub fn read_cached(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, Fail> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => return Ok(None),
        Ok(m) if m.is_file() && m.len() <= limit => {}
        _ => return Ok(None),
    }
    Ok(Some(std::fs::read(path)?))
}

/// The private atomic publish used by `_cache_png`, `_ink_inventory` and the
/// thumbnail writer: `O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW` 0600 temporary,
/// optional fsync, guards before writing and before `os.replace`; the
/// temporary is always removed.
pub fn write_private(path: &Path, data: &[u8], hex_bytes: usize, fsync: bool,
                     guard: &dyn Fn() -> Result<(), Fail>) -> Result<(), Fail> {
    let temporary = partial_path(path, hex_bytes);
    let result = (|| -> Result<(), Fail> {
        guard()?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&temporary)?;
        file.write_all(data)?;
        file.flush()?;
        if fsync {
            file.sync_all()?;
        }
        drop(file);
        guard()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

/// `json.dumps(str)` with `ensure_ascii=True`.
pub fn json_str(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
        }
    }
    out.push('"');
}

/// `float.__repr__`
pub fn float_repr(v: f64) -> String {
    rs_editor::pyval::py_float_repr(v)
}

/// A small Python-JSON value used for renderer input and cache files.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i128),
    /// An int already in canonical decimal form (arbitrary size).
    RawInt(String),
    Float(f64),
    Str(String),
    List(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn obj(items: Vec<(&str, J)>) -> J {
        J::Obj(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// `json.dumps(value, allow_nan=False, separators=sep)`; Err on NaN/inf.
    pub fn dump(&self, out: &mut String, compact: bool) -> Result<(), String> {
        let (item, key) = if compact { (",", ":") } else { (", ", ": ") };
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => out.push_str(&i.to_string()),
            J::RawInt(s) => out.push_str(s),
            J::Float(f) => {
                if !f.is_finite() {
                    return Err(format!("Out of range float values are not JSON compliant: {}", float_repr(*f)));
                }
                out.push_str(&float_repr(*f));
            }
            J::Str(s) => json_str(out, s),
            J::List(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(item);
                    }
                    v.dump(out, compact)?;
                }
                out.push(']');
            }
            J::Obj(items) => {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(item);
                    }
                    json_str(out, k);
                    out.push_str(key);
                    v.dump(out, compact)?;
                }
                out.push('}');
            }
        }
        Ok(())
    }

    pub fn dumps(&self, compact: bool) -> Result<String, String> {
        let mut out = String::new();
        self.dump(&mut out, compact)?;
        Ok(out)
    }
}
