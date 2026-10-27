//! Replacement for the Python module `ui_assets`: optional local Windows fonts,
//! never a general file-serving endpoint. Core logic is pure Rust.
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::PyBytes;
use sha2::{Digest, Sha256};

use crate::pyjson::{self, JValue};

pub const FONT_NAMES: [&str; 5] = ["segoeui.ttf", "segoeuib.ttf", "segoeuil.ttf", "seguisb.ttf", "segoeuisl.ttf"];
pub const MAX_FONT_BYTES: usize = 2 * 1024 * 1024;

/// `_private_read`: O_NOFOLLOW regular file, owned by us, mode & 077 == 0, 0 < size <= limit.
pub fn private_read(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a NUL-terminated CString that outlives the call; open has no other preconditions.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` was just returned by a successful open (>= 0) and is owned by nothing else; the File takes sole ownership and closes it on drop.
    let file: std::fs::File = unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) };
    let meta = file.metadata().ok()?;
    // SAFETY: getuid takes no arguments, has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !meta.file_type().is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 || !(0 < meta.size() && meta.size() <= limit as u64) {
        return None;
    }
    let mut raw = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut raw).ok()?;
    if raw.len() > limit {
        return None;
    }
    Some(raw)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Return only a named, manifest-verified TrueType asset, or None.
pub fn local_font(directory: &Path, name: &str) -> Option<Vec<u8>> {
    if !FONT_NAMES.contains(&name) {
        return None;
    }
    let meta = std::fs::symlink_metadata(directory).ok()?;
    // SAFETY: getuid takes no arguments, has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !meta.file_type().is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return None;
    }
    let raw_manifest = private_read(&directory.join("manifest.json"), 16384)?;
    // .decode('utf-8-sig') then json.loads(str)
    let text = std::str::from_utf8(&raw_manifest).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let manifest = pyjson::loads_str_lenient(text).ok()?;
    let JValue::Object(_) = &manifest else { return None };
    if !pyjson::eq_int(manifest.get("schemaVersion"), 1) {
        return None;
    }
    let Some(JValue::Array(files)) = manifest.get("files") else { return None };
    let matches: Vec<&JValue> = files
        .iter()
        .filter(|e| matches!(e, JValue::Object(_)) && matches!(e.get("file"), Some(JValue::Str(s)) if s.as_str() == Some(name)))
        .collect();
    if matches.len() != 1 {
        return None;
    }
    let raw = private_read(&directory.join(name), MAX_FONT_BYTES)?;
    let entry = matches[0];
    let bytes_ok = pyjson::eq_int(entry.get("bytes"), raw.len() as i64);
    let sha_ok = matches!(entry.get("sha256"), Some(JValue::Str(s)) if s.as_str() == Some(hex(&Sha256::digest(&raw)).as_str()));
    if raw.len() < 12 || raw[..4] != [0, 1, 0, 0] || !bytes_ok || !sha_ok {
        return None;
    }
    Some(raw)
}

#[cfg(feature = "python")]
#[pyfunction(name = "local_font")]
fn py_local_font<'py>(py: Python<'py>, directory: PathBuf, name: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyBytes>>> {
    // Non-str names are never in FONT_NAMES (Python: `name not in FONT_NAMES`).
    let Ok(name) = name.extract::<String>() else { return Ok(None) };
    let found = py.detach(|| local_font(&directory, &name));
    Ok(found.map(|raw| PyBytes::new(py, &raw)))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_local_font, m)?)?;
    rs_common::export(m, &["local_font"])
}
