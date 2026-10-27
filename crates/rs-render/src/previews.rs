//! Replacement for the Python module `previews`.
//!
//! The queue/worker orchestration keeps Python objects (threading, queue,
//! the engine) because callers and tests drive it through them; all pixel
//! work, PNG validation, URL checks and cache writes are Rust.
#[cfg(feature = "python")]
use crate::dataclasses::{is_ink, is_pdf};
use crate::pdf_background;
use crate::pixels as px;
#[cfg(feature = "python")]
use crate::pyhelp::{self, kwargs, with_cm, with_lock};
use crate::xarea;
#[cfg(feature = "python")]
use pyo3::exceptions::PyException;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyCFunction, PyDict, PyList, PyTuple};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const MODULE: &str = "samsung_notes.previews";
pub const BROKER_HOSTS: &[&str] = &["ew1a-api.samsungcloud.com"];
pub const FILE_HOSTS: &[&str] = &["eu1rgn-down.file.samsungcloud.com"];
const MAX_QUEUED: usize = 256;
const DEFAULT_MESSAGE: &str = "Bu sayfanın görüntüsü henüz açılamıyor.";
const GENERIC_FAILURE: &str = "Sayfa görüntüsü hazırlanamadı. Metin önizlemesi korunuyor.";

#[cfg(feature = "python")]
fn g<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    pyhelp::global(py, MODULE, name)
}

// ---------------------------------------------------------------- errors

#[cfg(feature = "python")]
#[pyclass(extends=PyException, subclass, module="samsung_notes.previews", name="PreviewError")]
pub struct PreviewError {
    #[pyo3(get, set)]
    message: Py<PyAny>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PreviewError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(py: Python<'_>, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        PreviewError { message: DEFAULT_MESSAGE.into_pyobject(py).unwrap().into_any().unbind() }
    }

    #[cfg(feature = "python")]
    #[pyo3(signature=(message=None))]
    fn __init__(slf: &Bound<'_, Self>, message: Option<Bound<'_, PyAny>>) -> PyResult<()> {
        let message = match message {
            Some(m) => m,
            None => DEFAULT_MESSAGE.into_pyobject(slf.py())?.into_any(),
        };
        slf.setattr("args", (message.clone(),))?;
        slf.borrow_mut().message = message.unbind();
        Ok(())
    }
}

#[cfg(feature = "python")]
pub fn preview_error(py: Python<'_>, message: &str) -> PyErr {
    match py.get_type::<PreviewError>().call1((message,)) {
        Ok(value) => PyErr::from_value(value),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------- URLs

/// The parts of urllib.parse.urlsplit the origin allowlist depends on.
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub fragment: String,
}

pub fn urlsplit(url: &str) -> SplitUrl {
    let mut scheme = String::new();
    let mut rest = url;
    if let Some(i) = url.find(':') {
        let candidate = &url[..i];
        let first = candidate.chars().next();
        if i > 0 && first.is_some_and(|c| c.is_ascii_alphabetic())
            && candidate.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            scheme = candidate.to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        rest = &after[end..];
    }
    let mut fragment = String::new();
    if let Some(i) = rest.find('#') {
        fragment = rest[i + 1..].to_string();
        rest = &rest[..i];
    }
    if let Some(i) = rest.find('?') {
        rest = &rest[..i];
    }
    SplitUrl { scheme, netloc, path: rest.to_string(), fragment }
}

fn hostname(netloc: &str) -> String {
    let host = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let host = if host.contains(']') {
        host.trim_start_matches('[').split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    host.to_lowercase()
}

/// previews._url core: Some(hostname) when `value` is an allowed https URL.
pub fn allowed_url(value: &str, mut allowed: impl FnMut(&str) -> bool) -> Option<String> {
    let length = value.chars().count();
    if !(0 < length && length <= 16384) || value.chars().any(|c| (c as u32) < 33) {
        return None;
    }
    let parts = urlsplit(value);
    if (parts.netloc.contains('[') != parts.netloc.contains(']')) || !parts.netloc.is_ascii() {
        return None; // urlsplit raises ValueError
    }
    let (user, password) = match parts.netloc.rsplit_once('@') {
        Some((info, _)) => match info.split_once(':') {
            Some((u, p)) => (u.to_string(), Some(p.to_string())),
            None => (info.to_string(), None),
        },
        None => (String::new(), None),
    };
    if parts.scheme != "https" || !allowed(&parts.netloc) || !user.is_empty()
        || password.is_some_and(|p| !p.is_empty()) || !parts.fragment.is_empty() || !parts.path.starts_with('/')
    {
        return None;
    }
    Some(hostname(&parts.netloc))
}

#[cfg(feature = "python")]
pub fn url_host(py: Python<'_>, value: &Bound<'_, PyAny>, hosts: &[&str]) -> PyResult<String> {
    let Ok(text) = value.extract::<String>() else { return Err(preview_error(py, DEFAULT_MESSAGE)) };
    if !value.is_instance_of::<pyo3::types::PyString>() {
        return Err(preview_error(py, DEFAULT_MESSAGE));
    }
    allowed_url(&text, |n| hosts.contains(&n)).ok_or_else(|| preview_error(py, DEFAULT_MESSAGE))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _url(py: Python<'_>, value: &Bound<'_, PyAny>, hosts: &Bound<'_, PyAny>) -> PyResult<String> {
    if !value.is_instance_of::<pyo3::types::PyString>() {
        return Err(preview_error(py, DEFAULT_MESSAGE));
    }
    let text: String = value.extract()?;
    let mut failure = None;
    let result = allowed_url(&text, |netloc| match hosts.contains(netloc) {
        Ok(found) => found,
        Err(e) => {
            failure = Some(e);
            false
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    result.ok_or_else(|| preview_error(py, DEFAULT_MESSAGE))
}

// ---------------------------------------------------------------- fetch / PNG

#[cfg(feature = "python")]
fn header_filter<'py>(py: Python<'py>, api: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let headers = PyDict::new(py);
    for item in api.call_method0("_headers")?.call_method0("items")?.try_iter()? {
        let (key, value): (Bound<PyAny>, Bound<PyAny>) = item?.extract()?;
        let lower: String = key.call_method0("lower")?.extract()?;
        if lower != "content-type" && lower != "content-encoding" {
            headers.set_item(key, value)?;
        }
    }
    Ok(headers)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature=(candidate, api, *, transport=None, guard=None))]
pub fn fetch_preview<'py>(py: Python<'py>, candidate: &Bound<'py, PyAny>, api: &Bound<'py, PyAny>,
                          transport: Option<Bound<'py, PyAny>>, guard: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let check = || -> PyResult<()> {
        if let Some(g) = &guard {
            if !g.is_none() {
                g.call0()?;
            }
        }
        Ok(())
    };
    let transport = match transport {
        Some(t) => t,
        None => g(py, "_http")?,
    };
    check()?;
    let url = candidate.getattr("download_url")?;
    url_host(py, &url, BROKER_HOSTS)?;
    let headers = header_filter(py, api)?;
    check()?;
    let (mut status, mut raw, location): (Bound<PyAny>, Bound<PyAny>, Bound<PyAny>) = transport.call1((&url, headers))?.extract()?;
    if status.eq(302)? {
        url_host(py, &location, FILE_HOSTS)?;
        check()?;
        let accept = PyDict::new(py);
        accept.set_item("Accept", "application/octet-stream")?;
        let (s, r, _): (Bound<PyAny>, Bound<PyAny>, Bound<PyAny>) = transport.call1((&location, accept))?.extract()?;
        status = s;
        raw = r;
    }
    check()?;
    let bytes = raw.cast::<PyBytes>().ok().filter(|b| !status.ne(200).unwrap_or(true) && !b.as_bytes().is_empty()
        && b.as_bytes().len() <= px::MAX_INPUT);
    let Some(bytes) = bytes else {
        return Err(preview_error(py, "Sayfa görüntüsü alınamadı. Eşitleyip yeniden deneyebilirsin."));
    };
    let expected = candidate.getattr("sha256")?;
    if expected.ne(px::sha256_hex(bytes.as_bytes()))? {
        return Err(preview_error(py, "Sayfa dosyasının bütünlüğü doğrulanamadı."));
    }
    Ok(raw)
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _png_valid(raw: &Bound<'_, PyAny>, width: &Bound<'_, PyAny>, height: &Bound<'_, PyAny>) -> bool {
    match (raw.cast::<PyBytes>(), pyhelp::exact_int(width), pyhelp::exact_int(height)) {
        (Ok(raw), Some(w), Some(h)) => {
            let data = raw.as_bytes();
            raw.py().detach(|| px::png_valid(data, w, h))
        }
        _ => false,
    }
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _codec<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>, size: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let module = match py.import("samsung_notes.preview_codec") {
        Ok(m) => m,
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyImportError>(py) => {
            return Err(preview_error(py, "Sayfa görüntüleyicisi bu kurulumda bulunamadı."))
        }
        Err(e) => return Err(e),
    };
    let codec_error = module.getattr("PreviewCodecError")?;
    match module.getattr("decode_spi")?.call((raw,), Some(&kwargs(py, &[("expected_size", size.clone())])?)) {
        Ok(png) => Ok(png),
        Err(e) if e.is_instance(py, &codec_error) => Err(preview_error(py, "Bu sayfanın görüntüsü çözülemedi. Metin önizlemesi korunuyor.")),
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
fn swallow_import_os_value(py: Python<'_>, result: PyResult<bool>) -> PyResult<bool> {
    match result {
        Ok(v) => Ok(v),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyImportError>(py) || e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
            || e.is_instance_of::<pyo3::exceptions::PyValueError>(py) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn provider_available(py: Python<'_>) -> PyResult<bool> {
    swallow_import_os_value(py, (|| py.import("samsung_notes.preview_codec")?.getattr("provider_available")?.call0()?.is_truthy())())
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _ink_provider(py: Python<'_>) -> PyResult<bool> {
    swallow_import_os_value(py, (|| py.import("samsung_notes.ink_render")?.getattr("provider_available")?.call0()?.is_truthy())())
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _payload_from_snapshot<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let document_error = |m: &str| -> PyErr {
        xarea::document_error(py, m)
    };
    let Ok(dict) = value.cast::<PyDict>() else { return Err(document_error("Kayıtlı belge sürümü desteklenmiyor.")) };
    let version = dict.get_item("schemaVersion")?.unwrap_or_else(|| py.None().into_bound(py));
    if version.ne(1)? {
        return Err(document_error("Kayıtlı belge sürümü desteklenmiyor."));
    }
    let encoded = match dict.get_item("raw_body")? {
        Some(v) => v,
        None => "".into_pyobject(py)?.into_any(),
    };
    let limit: usize = (xarea::class(py, "tools.notes_payload", "MAX_BODY_BYTES")?.extract::<usize>()? + 2) / 3 * 4;
    if !encoded.is_instance_of::<pyo3::types::PyString>() || encoded.len()? > limit {
        return Err(document_error("Kayıtlı belge boyutu geçersiz."));
    }
    let raw = py.import("base64")?.getattr("b64decode")?.call((&encoded,), Some(&kwargs(py, &[("validate", true.into_pyobject(py)?.to_owned().into_any())])?))?;
    let digest = px::sha256_hex(raw.cast::<PyBytes>()?.as_bytes());
    let expected = dict.get_item("raw_sha256")?.unwrap_or_else(|| py.None().into_bound(py));
    if expected.ne(digest)? {
        return Err(document_error("Kayıtlı belgenin bütünlük kontrolü başarısız."));
    }
    let content_type = value.get_item("content_type")?;
    xarea::parse_payload(py)?.call((raw, content_type),
        Some(&kwargs(py, &[("max_parts", xarea::class(py, "tools.notes_payload", "MAX_READONLY_PARTS")?)])?))
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn _inventory_from_snapshot<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let payload = g(py, "_payload_from_snapshot")?.call1((value,))?;
    let inventory = xarea::ink_inventory(py, &payload)?;
    xarea::inventory_object(py, &inventory)
}

// ---------------------------------------------------------------- records

// ---------------------------------------------------------------- cache files

#[cfg(feature = "python")]
/// Atomic private write: O_EXCL|O_NOFOLLOW 0600 partial, fsync, then the
/// caller's guard, then rename. The partial file never survives.
fn write_private<'py>(py: Python<'py>, path: &Bound<'py, PyAny>, data: &[u8], guard: Option<&Bound<'py, PyAny>>, sync: bool) -> PyResult<()> {
    let temporary = path.call_method1("with_suffix", (format!(".partial-{}", pyhelp::token_hex(py, 8)?),))?;
    let target: PathBuf = py.import("os")?.getattr("fspath")?.call1((&temporary,))?.extract::<String>()?.into();
    let result = (|| -> PyResult<()> {
        if let Some(g) = guard {
            g.call0()?;
        }
        py.detach(|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW).open(&target)?;
            file.write_all(data)?;
            file.flush()?;
            if sync {
                file.sync_all()?;
            }
            Ok(())
        })?;
        if let Some(g) = guard {
            g.call0()?;
        }
        py.import("os")?.getattr("replace")?.call1((&temporary, path))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&target);
    result
}

#[cfg(feature = "python")]
fn read_cached<'py>(py: Python<'py>, path: &Bound<'py, PyAny>, limit: usize) -> PyResult<Option<Vec<u8>>> {
    let local: PathBuf = py.import("os")?.getattr("fspath")?.call1((path,))?.extract::<String>()?.into();
    Ok(py.detach(|| {
        let meta = std::fs::symlink_metadata(&local).ok()?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() as usize > limit {
            return None;
        }
        std::fs::read(&local).ok()
    }))
}

// ---------------------------------------------------------------- manager

#[cfg(feature = "python")]
/// PreviewManager._fetch_pdf_refreshing(self, identity, candidate, guard).
/// Download a PDF background; signed URLs in a stored snapshot expire, so on
/// a refused link read the note once from Cloud (read-only), store it and
/// retry once. `self` is duck-typed (engine, directory), like the Python
/// method called unbound by the oracle.
#[pyfunction]
fn _fetch_pdf_refreshing<'py>(py: Python<'py>, manager: &Bound<'py, PyAny>, identity: &Bound<'py, PyAny>,
                              candidate: &Bound<'py, PyAny>, guard: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let engine = manager.getattr("engine")?;
    let store = engine.getattr("store")?;
    let account = store.call_method1("meta", ("account_key", "offline"))?;
    let directory = manager.getattr("directory")?.div("pdf-files")?;
    let fetch = |item: &Bound<'py, PyAny>| -> PyResult<Bound<'py, PyAny>> {
        g(py, "fetch_pdf")?.call((item, engine.call_method0("_api")?, &directory),
            Some(&kwargs(py, &[("account", account.clone()), ("guard", guard.clone())])?))
    };
    match fetch(candidate) {
        Err(e) if e.is_instance(py, &py.get_type::<pdf_background::PdfLinkExpired>()) => {
            let server = store.call_method1("raw", (identity,))?.get_item("server_id")?;
            let fresh = engine.call_method0("_api")?.call_method1("get_note", (&server,))?;
            store.call_method1("apply_document", (&server, fresh.call_method0("to_dict")?))?;
            let payload = g(py, "_payload_from_snapshot")?.call1((fresh.call_method0("to_dict")?,))?;
            let mut renewed = None;
            for c in g(py, "inspect_pdf_backgrounds")?.call1((payload,))?.try_iter()? {
                let c = c?;
                if c.getattr("page_index")?.eq(candidate.getattr("page_index")?)? && c.getattr("sha256")?.eq(candidate.getattr("sha256")?)? {
                    renewed = Some(c);
                    break;
                }
            }
            let renewed = match renewed {
                Some(r) if r.getattr("download_url")?.is_truthy()? => r,
                _ => return Err(pdf_background::error(py, "PDF sayfası notta değişti; sayfayı yeniden açabilirsin.")),
            };
            guard.call0()?;
            fetch(&renewed)
        }
        other => other,
    }
}

#[cfg(feature = "python")]
#[pyclass(subclass, dict, module="samsung_notes.previews", name="PreviewManager")]
pub struct PreviewManager {}

#[cfg(feature = "python")]
fn store_error(py: Python<'_>, code: &str, message: &str, status: u16) -> PyErr {
    xarea::store_error(py, code, message, status)
}

#[cfg(feature = "python")]
fn sessions<'py>(engine: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    engine.getattr("sessions")
}

#[cfg(feature = "python")]
#[pymethods]
impl PreviewManager {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        PreviewManager {}
    }

    #[cfg(feature = "python")]
    #[pyo3(signature=(engine, *, decoder=None, fetcher=None))]
    fn __init__(slf: &Bound<'_, Self>, engine: Bound<'_, PyAny>, decoder: Option<Bound<'_, PyAny>>, fetcher: Option<Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let module = py.import(MODULE)?;
        let decoder = match decoder { Some(d) => d, None => module.getattr("_codec")? };
        let fetcher = match fetcher { Some(f) => f, None => module.getattr("fetch_preview")? };
        slf.setattr("engine", &engine)?;
        slf.setattr("decoder", decoder)?;
        slf.setattr("fetcher", fetcher)?;
        let directory = engine.getattr("store")?.getattr("directory")?.div("page-previews")?;
        slf.setattr("directory", xarea::private_directory(py, &directory)?)?;
        let threading = py.import("threading")?;
        let lock = threading.getattr("RLock")?.call0()?;
        slf.setattr("finished", threading.getattr("Condition")?.call1((&lock,))?)?;
        slf.setattr("lock", lock)?;
        slf.setattr("entries", PyDict::new(py))?;
        slf.setattr("jobs", PyDict::new(py))?;
        slf.setattr("queue", py.import("queue")?.getattr("PriorityQueue")?.call0()?)?;
        slf.setattr("sequence", py.import("itertools")?.getattr("count")?.call0()?)?;
        slf.setattr("stop", threading.getattr("Event")?.call0()?)?;
        slf.setattr("workers", PyList::empty(py))?;
        Ok(())
    }

    #[cfg(feature = "python")]
    fn _source<'py>(slf: &Bound<'py, Self>, identity: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let py = slf.py();
        let engine = slf.getattr("engine")?;
        let row = with_cm(&engine.getattr("store")?.call_method0("connect")?, |db| {
            db.call_method1("execute", ("SELECT id,remote_commit,loaded,deleted_remote,revision,modified_ms FROM notes WHERE id=?", (identity,)))?
                .call_method0("fetchone")
        })?;
        if row.is_none() {
            return Err(store_error(py, "not_found", "Not bulunamadı.", 404));
        }
        let mut items: Vec<Bound<PyAny>> = row.try_iter()?.collect::<PyResult<_>>()?;
        let sessions = engine.getattr_opt("sessions")?;
        let (path, offline) = match &sessions {
            Some(s) => (s.getattr_opt("path")?.unwrap_or_else(|| py.None().into_bound(py)),
                        s.getattr_opt("offline")?.map(|o| o.is_truthy()).transpose()?.unwrap_or(false)),
            None => (py.None().into_bound(py), false),
        };
        items.push(path);
        items.push(offline.into_pyobject(py)?.to_owned().into_any());
        PyTuple::new(py, items)
    }

    #[cfg(feature = "python")]
    fn _entry<'py>(slf: &Bound<'py, Self>, identity: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let source = slf.call_method1("_source", (identity,))?;
        let lock = slf.getattr("lock")?;
        let entries = slf.getattr("entries")?;
        let cached = with_lock(&lock, || -> PyResult<Option<Bound<PyAny>>> {
            if entries.contains(identity)? {
                let entry = entries.get_item(identity)?;
                if entry.getattr("source")?.eq(&source)? {
                    return Ok(Some(entry));
                }
            }
            Ok(None)
        })?;
        if let Some(entry) = cached {
            return Ok(entry);
        }
        let engine = slf.getattr("engine")?;
        let row = engine.getattr("store")?.call_method1("raw", (identity,))?;
        let candidates = PyDict::new(py);
        let pages = PyList::empty(py);
        if row.get_item("base_document")?.is_truthy()? && row.get_item("loaded")?.is_truthy()? && !row.get_item("deleted_remote")?.is_truthy()? {
            let outcome = Self::inventory(slf, &row, &candidates, &pages);
            if let Err(e) = outcome {
                let tolerated = e.is_instance(py, &xarea::class(py, "samsung_notes.document", "DocumentError")?)
                    || e.is_instance(py, &xarea::class(py, "samsung_notes.ink", "InkError")?)
                    || e.is_instance_of::<pyo3::exceptions::PyValueError>(py) || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
                    || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py);
                if !tolerated {
                    return Err(e);
                }
            }
        }
        let entry = g(py, "_Entry")?.call1((&source, PyTuple::new(py, pages.iter())?, &candidates))?;
        with_lock(&lock, || {
            if entries.len()? >= 128 {
                let first = entries.try_iter()?.next().transpose()?;
                if let Some(first) = first {
                    entries.call_method1("pop", (first,))?;
                }
            }
            entries.set_item(identity, &entry)
        })?;
        Ok(entry)
    }

    #[cfg(feature = "python")]
    fn _ink_inventory<'py>(slf: &Bound<'py, Self>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let inspect = py.import("samsung_notes.ink_render")?.getattr("inspect_render_pages")?;
        let recipe: String = g(py, "INK_INVENTORY_RECIPE")?.extract()?;
        let sha: String = snapshot.get_item("raw_sha256")?.extract()?;
        let key = px::sha256_hex(format!("{sha}:{recipe}").as_bytes());
        let directory = xarea::private_directory(py, &slf.getattr("directory")?.div("ink-inventory")?)?;
        let path = directory.div(format!("{key}.json"))?;
        if let Ok(Some(raw)) = read_cached(py, &path, 1024 * 1024) {
            let loaded = String::from_utf8(raw).ok().and_then(|text| py.import("json").ok()?.getattr("loads").ok()?.call1((text,)).ok());
            if let Some(pages) = loaded {
                if let Ok(list) = pages.cast::<PyList>() {
                    let wanted = ["darkMode", "height", "index", "width"];
                    let valid = list.iter().all(|p| p.cast::<PyDict>().is_ok_and(|d| {
                        let mut keys: Vec<String> = d.keys().iter().filter_map(|k| k.extract().ok()).collect();
                        keys.sort();
                        d.len() == 4 && keys == wanted
                    }));
                    if valid {
                        return Ok(PyTuple::new(py, list.iter())?.into_any());
                    }
                }
            }
        }
        let pages = inspect.call1((snapshot,))?;
        let text: String = py.import("json")?.getattr("dumps")?.call1((PyList::new(py, pages.try_iter()?.collect::<PyResult<Vec<_>>>()?)?,))?.extract()?;
        match write_private(py, &path, text.as_bytes(), None, false) {
            Err(e) if e.is_instance_of::<pyo3::exceptions::PyOSError>(py) => {}
            other => other?,
        }
        Ok(pages)
    }

    #[cfg(feature = "python")]
    fn manifest<'py>(slf: &Bound<'py, Self>, identity: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let entry = slf.call_method1("_entry", (identity,))?;
        let candidates = entry.getattr("candidates")?;
        let values: Vec<Bound<PyAny>> = candidates.call_method0("values")?.try_iter()?.collect::<PyResult<_>>()?;
        let pdf = values.iter().any(|c| is_pdf(c));
        let ink = values.iter().any(|c| is_ink(c));
        let other = values.iter().any(|c| !is_pdf(c) && !is_ink(c));
        let mut available = if pdf { g(py, "pdf_provider_available")?.call0()?.is_truthy()? } else { false };
        if ink {
            available = available || g(py, "_ink_provider")?.call0()?.is_truthy()?;
        }
        if other || values.is_empty() {
            available = available || g(py, "provider_available")?.call0()?.is_truthy()?;
        }
        let result = PyDict::new(py);
        result.set_item("pages", PyList::new(py, entry.getattr("pages")?.try_iter()?.collect::<PyResult<Vec<_>>>()?)?)?;
        result.set_item("providerAvailable", available)?;
        Ok(result)
    }

    #[cfg(feature = "python")]
    fn _path<'py>(slf: &Bound<'py, Self>, candidate: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let engine = slf.getattr("engine")?;
        let account: String = engine.getattr("store")?.call_method1("meta", ("account_key", "offline"))?.extract()?;
        let dumps = py.import("json")?.getattr("dumps")?;
        let mut recipe = String::from(":native-preview-v1");
        let dark = candidate.getattr_opt("is_dark_mode")?.map(|d| d.is_truthy()).transpose()?.unwrap_or(false);
        let is_pdf = is_pdf(candidate);
        let is_ink = is_ink(candidate);
        if dark && !is_pdf && !is_ink {
            recipe.push_str(":light-v1");
        }
        if is_pdf {
            let overlay = candidate.getattr("overlay")?;
            let overlay_sha = if overlay.is_truthy()? { overlay.getattr("sha256")? } else { py.None().into_bound(py) };
            let key = PyTuple::new(py, [candidate.getattr("pdf_page_index")?, candidate.getattr("width")?, candidate.getattr("height")?,
                                       candidate.getattr("rect")?, overlay_sha])?;
            recipe = format!(":pdf-background-v1:{}", dumps.call1((key,))?.extract::<String>()?);
        } else if is_ink {
            let key = PyTuple::new(py, [candidate.getattr("page_index")?, candidate.getattr("width")?, candidate.getattr("height")?])?;
            recipe = format!(":ink-reconstructed-v2:{}", dumps.call1((key,))?.extract::<String>()?);
        }
        let sha: String = candidate.getattr("sha256")?.extract()?;
        let key = px::sha256_hex(format!("{account}:{sha}{recipe}").as_bytes());
        slf.getattr("directory")?.div(format!("{key}.png"))
    }

    #[cfg(feature = "python")]
    #[pyo3(signature=(identity, page_index, *, priority=None, wait=None))]
    fn request<'py>(slf: &Bound<'py, Self>, identity: &Bound<'py, PyAny>, page_index: &Bound<'py, PyAny>,
                    priority: Option<Bound<'py, PyAny>>, wait: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let priority = match priority { Some(p) => p, None => 0i64.into_pyobject(py)?.into_any() };
        let wait = wait.unwrap_or(0.0);
        let deadline = pyhelp::monotonic(py)? + wait;
        loop {
            let raw = slf.call_method1("_request", (identity, page_index, &priority))?;
            if !raw.is_none() || wait <= 0.0 {
                return Ok(raw);
            }
            let remaining = deadline - pyhelp::monotonic(py)?;
            if remaining <= 0.0 {
                return Ok(py.None().into_bound(py));
            }
            let finished = slf.getattr("finished")?;
            with_lock(&finished, || finished.call_method1("wait", (remaining.min(1.0),)).map(|_| ()))?;
        }
    }

    #[cfg(feature = "python")]
    fn _request<'py>(slf: &Bound<'py, Self>, identity: &Bound<'py, PyAny>, page_index: &Bound<'py, PyAny>,
                     priority: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let none = py.None().into_bound(py);
        let entry = slf.call_method1("_entry", (identity,))?;
        let candidate = entry.getattr("candidates")?.call_method1("get", (page_index,))?;
        if candidate.is_none() {
            return Err(store_error(py, "preview_unavailable", "Bu sayfa için görüntü önizlemesi bulunmuyor.", 404));
        }
        let path = slf.call_method1("_path", (&candidate,))?;
        let (width, height) = (candidate.getattr("width")?, candidate.getattr("height")?);
        if let Some(raw) = read_cached(py, &path, px::MAX_OUTPUT)? {
            let valid = match (pyhelp::exact_int(&width), pyhelp::exact_int(&height)) {
                (Some(w), Some(h)) => py.detach(|| px::png_valid(&raw, w, h)),
                _ => false,
            };
            if valid {
                return Ok(PyBytes::new(py, &raw).into_any());
            }
        }
        let key = path.getattr("name")?;
        let engine = slf.getattr("engine")?;
        let lock = slf.getattr("lock")?;
        let jobs = slf.getattr("jobs")?;
        let queue = slf.getattr("queue")?;
        let sequence = slf.getattr("sequence")?;
        let is_ink = is_ink(&candidate);
        with_lock(&lock, || -> PyResult<()> {
            let mut job = jobs.call_method1("get", (&key,))?;
            if !job.is_none() {
                let state: String = job.get_item("state")?.extract()?;
                if state == "pending" || state == "running" {
                    if state == "pending" && priority.lt(job.get_item("priority")?)? {
                        job.set_item("priority", priority)?;
                        let item = PyTuple::new(py, [priority.clone(), sequence.call_method0("__next__")?, key.clone(), identity.clone(),
                            entry.getattr("source")?, candidate.clone(), path.clone(), sessions(&engine)?.getattr("path")?, job.get_item("local_pdf")?])?;
                        queue.call_method1("put", (item,))?;
                    }
                    return Ok(());
                }
                if state == "ready" {
                    jobs.call_method1("pop", (&key,))?;
                    job = none.clone();
                } else if state == "error" {
                    let retry = job.call_method1("get", ("retry_after", 0))?;
                    if retry.gt(pyhelp::monotonic(py)?)? {
                        let message: String = job.get_item("error")?.extract()?;
                        return Err(store_error(py, "preview_unavailable", &message, 422));
                    }
                    jobs.call_method1("pop", (&key,))?;
                    job = none.clone();
                }
            }
            if job.is_none() {
                let account = engine.getattr("store")?.call_method1("meta", ("account_key", "offline"))?;
                let local_pdf = if is_pdf(&candidate) {
                    g(py, "cached_pdf")?.call((&candidate, slf.getattr("directory")?.div("pdf-files")?), Some(&kwargs(py, &[("account", account)])?))?
                } else {
                    none.clone()
                };
                let session = sessions(&engine)?;
                if (!session.getattr("path")?.is_truthy()? || session.getattr("offline")?.is_truthy()?) && local_pdf.is_none() && !is_ink {
                    return Err(store_error(py, "preview_offline", "Bu sayfa henüz indirilmemiş. Bağlantı kurduğunda tekrar açabilirsin.", 503));
                }
                let mut active = 0;
                let mut finished_name = None;
                for item in jobs.call_method0("items")?.try_iter()? {
                    let (name, value): (Bound<PyAny>, Bound<PyAny>) = item?.extract()?;
                    let state: String = value.get_item("state")?.extract()?;
                    if state == "pending" || state == "running" {
                        active += 1;
                    } else if finished_name.is_none() {
                        finished_name = Some(name);
                    }
                }
                if active >= MAX_QUEUED {
                    return Err(store_error(py, "preview_busy", "Diğer sayfalar hazırlanıyor. Biraz sonra tekrar açabilirsin.", 429));
                }
                if jobs.len()? >= 4 * MAX_QUEUED {
                    if let Some(name) = finished_name {
                        jobs.call_method1("pop", (name,))?;
                    }
                }
                let record = PyDict::new(py);
                record.set_item("state", "pending")?;
                record.set_item("error", &none)?;
                record.set_item("priority", priority)?;
                record.set_item("local_pdf", &local_pdf)?;
                jobs.set_item(&key, record)?;
                let item = PyTuple::new(py, [priority.clone(), sequence.call_method0("__next__")?, key.clone(), identity.clone(),
                    entry.getattr("source")?, candidate.clone(), path.clone(), session.getattr("path")?, local_pdf])?;
                queue.call_method1("put", (item,))?;
                let workers = slf.getattr("workers")?;
                let limit: usize = g(py, "WORKERS")?.extract()?;
                let threading = py.import("threading")?;
                while workers.len()? < limit {
                    let worker = threading.getattr("Thread")?.call((), Some(&kwargs(py, &[
                        ("target", slf.getattr("_loop")?), ("daemon", true.into_pyobject(py)?.to_owned().into_any()),
                        ("name", format!("notes-page-preview-{}", workers.len()?).into_pyobject(py)?.into_any())])?))?;
                    workers.call_method1("append", (&worker,))?;
                    worker.call_method0("start")?;
                }
            }
            Ok(())
        })?;
        Ok(none)
    }

    #[cfg(feature = "python")]
    #[pyo3(signature=(limit=None))]
    fn prefetch(slf: &Bound<'_, Self>, limit: Option<usize>) -> PyResult<usize> {
        let py = slf.py();
        let limit = limit.unwrap_or(MAX_QUEUED / 2);
        let engine = slf.getattr("engine")?;
        let rows = with_cm(&engine.getattr("store")?.call_method0("connect")?, |db| {
            db.call_method1("execute", ("SELECT id FROM notes WHERE loaded=1 AND deleted_remote=0 AND trashed=0 ORDER BY modified_ms DESC",))?
                .call_method0("fetchall")
        })?;
        let store_error_class = xarea::class(py, "samsung_notes.storage", "StoreError")?;
        let stop = slf.getattr("stop")?;
        let mut queued = 0;
        for row in rows.try_iter()? {
            if queued >= limit || stop.call_method0("is_set")?.is_truthy()? {
                break;
            }
            let identity = row?.get_item("id")?;
            let step = (|| -> PyResult<()> {
                let entry = slf.call_method1("_entry", (&identity,))?;
                if entry.getattr("candidates")?.call_method1("get", (0,))?.is_none() {
                    return Ok(());
                }
                if slf.call_method1("_request", (&identity, 0, 2))?.is_none() {
                    queued += 1;
                }
                Ok(())
            })();
            match step {
                Err(e) if e.is_instance(py, &store_error_class) => continue,
                other => other?,
            }
        }
        Ok(queued)
    }

    #[cfg(feature = "python")]
    /// Bound through functools.partialmethod so the unbound call
    /// `PreviewManager._fetch_pdf_refreshing(obj, ...)` accepts any `obj`.
    #[classattr]
    fn _fetch_pdf_refreshing(py: Python<'_>) -> PyResult<Py<PyAny>> {
        let module = pyo3::types::PyModule::new(py, "_previews_helpers")?;
        let function = wrap_pyfunction!(_fetch_pdf_refreshing, &module)?;
        Ok(py.import("functools")?.getattr("partialmethod")?.call1((function,))?.unbind())
    }

    #[cfg(feature = "python")]
    #[staticmethod]
    fn _cache_png<'py>(py: Python<'py>, path: &Bound<'py, PyAny>, png: &Bound<'py, PyAny>, width: &Bound<'py, PyAny>,
                       height: &Bound<'py, PyAny>, guard: &Bound<'py, PyAny>) -> PyResult<()> {
        if !_png_valid(png, width, height) {
            return Err(preview_error(py, "Sayfa görüntüsünün boyutu doğrulanamadı."));
        }
        write_private(py, path, png.cast::<PyBytes>()?.as_bytes(), Some(guard), true)
    }

    #[cfg(feature = "python")]
    fn _loop(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let stop = slf.getattr("stop")?;
        let queue = slf.getattr("queue")?;
        let empty = py.import("queue")?.getattr("Empty")?;
        while !stop.call_method0("is_set")?.is_truthy()? {
            let item = match queue.call_method("get", (), Some(&kwargs(py, &[("timeout", 0.5f64.into_pyobject(py)?.into_any())])?)) {
                Ok(item) => item,
                Err(e) if e.is_instance(py, &empty) => continue,
                Err(e) => return Err(e),
            };
            let fields: Vec<Bound<PyAny>> = item.try_iter()?.collect::<PyResult<_>>()?;
            let [priority, _, key, identity, source, candidate, path, session, local_pdf]: [Bound<PyAny>; 9] =
                fields.try_into().map_err(|_| pyo3::exceptions::PyValueError::new_err("queue item"))?;
            let lock = slf.getattr("lock")?;
            let jobs = slf.getattr("jobs")?;
            let claimed = with_lock(&lock, || -> PyResult<bool> {
                let job = jobs.call_method1("get", (&key,))?;
                if job.is_none() || job.get_item("state")?.ne("pending")? || job.get_item("priority")?.ne(&priority)? {
                    queue.call_method0("task_done")?;
                    return Ok(false);
                }
                job.set_item("state", "running")?;
                Ok(true)
            })?;
            if !claimed {
                continue;
            }
            let outcome = Self::process(slf, &key, &identity, &source, &candidate, &path, &session, &local_pdf);
            let failure = match outcome {
                Ok(()) => None,
                Err(e) => Some(e),
            };
            let mut fatal = None;
            let record = PyDict::new(py);
            match failure {
                None => {
                    record.set_item("state", "ready")?;
                    record.set_item("error", py.None())?;
                }
                Some(e) => {
                    let known = [g(py, "PreviewError")?, g(py, "PdfBackgroundError")?, xarea::class(py, "samsung_notes.cloud_api", "APIError")?, xarea::class(py, "samsung_notes.storage", "StoreError")?]
                        .iter().any(|class| e.is_instance(py, class));
                    let message = if known {
                        e.value(py).getattr("message")?
                    } else {
                        if !e.is_instance_of::<PyException>(py) {
                            fatal = Some(e);
                        }
                        GENERIC_FAILURE.into_pyobject(py)?.into_any()
                    };
                    record.set_item("state", "error")?;
                    record.set_item("error", message)?;
                    record.set_item("retry_after", pyhelp::monotonic(py)? + 5.0)?;
                }
            }
            if fatal.is_none() {
                with_lock(&lock, || jobs.set_item(&key, &record))?;
            }
            let finished = slf.getattr("finished")?;
            with_lock(&finished, || finished.call_method0("notify_all").map(|_| ()))?;
            queue.call_method0("task_done")?;
            if let Some(e) = fatal {
                return Err(e);
            }
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    fn close(slf: &Bound<'_, Self>) -> PyResult<()> {
        slf.getattr("stop")?.call_method0("set")?;
        Ok(())
    }

    #[cfg(feature = "python")]
    #[pyo3(signature=(timeout=None))]
    fn join(slf: &Bound<'_, Self>, timeout: Option<Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let timeout = timeout.unwrap_or_else(|| py.None().into_bound(py));
        let workers: Vec<Bound<PyAny>> = slf.getattr("workers")?.try_iter()?.collect::<PyResult<_>>()?;
        for worker in workers {
            worker.call_method1("join", (&timeout,))?;
        }
        Ok(())
    }
}

#[cfg(feature = "python")]
impl PreviewManager {
    #[cfg(feature = "python")]
    /// The inventory part of `_entry`; errors are filtered by the caller.
    fn inventory<'py>(slf: &Bound<'py, Self>, row: &Bound<'py, PyAny>, candidates: &Bound<'py, PyDict>, pages: &Bound<'py, PyList>) -> PyResult<()> {
        let py = slf.py();
        let json_loads = py.import("json")?.getattr("loads")?;
        let base = row.get_item("base_document")?;
        let payload = g(py, "_payload_from_snapshot")?.call1((json_loads.call1((&base,))?,))?;
        let inventory = xarea::ink_inventory(py, &payload)?;
        let mut sorted: Vec<(bool, i64, Bound<PyAny>)> = Vec::new();
        for candidate in xarea::candidates(py, &inventory)? {
            let dark = candidate.getattr("is_dark_mode")?.is_truthy()?;
            let area: i64 = candidate.getattr("width")?.extract::<i64>()? * candidate.getattr("height")?.extract::<i64>()?;
            sorted.push((dark, -area, candidate));
        }
        sorted.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        for (_, negative_area, candidate) in sorted {
            let index = candidate.getattr("page_index")?;
            if candidate.getattr("download_url")?.is_truthy()? && -negative_area <= px::MAX_PIXELS && !candidates.contains(&index)? {
                candidates.set_item(index, candidate)?;
            }
        }
        let pdf_pages = PyDict::new(py);
        let mut unsupported = py.import("builtins")?.getattr("frozenset")?.call0()?;
        let pdf_error = g(py, "PdfBackgroundError")?;
        let pdf_step = (|| -> PyResult<()> {
            for candidate in g(py, "inspect_pdf_backgrounds")?.call1((&payload,))?.try_iter()? {
                let candidate = candidate?;
                let index = candidate.getattr("page_index")?;
                pdf_pages.call_method1("setdefault", (&index, PyList::empty(py)))?.call_method1("append", (candidate,))?;
            }
            let can_decode_overlay = if pdf_pages.len() > 0 { g(py, "provider_available")?.call0()?.is_truthy()? } else { false };
            for (index, placements) in pdf_pages.iter() {
                if placements.len()? != 1 {
                    continue;
                }
                let mut pdf = placements.get_item(0)?;
                if !pdf.getattr("download_url")?.is_truthy()? {
                    continue;
                }
                let overlay = candidates.get_item(&index)?;
                if let Some(overlay) = overlay {
                    if pdf.getattr("annotation_objects")?.is_truthy()? && !overlay.getattr("is_dark_mode")?.is_truthy()?
                        && overlay.getattr("width")?.eq(pdf.getattr("width")?)? && overlay.getattr("height")?.eq(pdf.getattr("height")?)?
                    {
                        let engine = slf.getattr("engine")?;
                        let sessions = engine.getattr_opt("sessions")?;
                        let connected = match &sessions {
                            Some(s) => s.getattr_opt("path")?.map(|p| p.is_truthy()).transpose()?.unwrap_or(false)
                                && !s.getattr_opt("offline")?.map(|o| o.is_truthy()).transpose()?.unwrap_or(false),
                            None => false,
                        };
                        let usable = (connected && can_decode_overlay)
                            || slf.call_method1("_path", (&overlay,))?.call_method0("is_file")?.is_truthy()?;
                        if usable {
                            pdf = crate::dataclasses::replace(&pdf, "overlay", overlay)?;
                        }
                    }
                }
                candidates.set_item(index, pdf)?;
            }
            Ok(())
        })();
        if let Err(e) = pdf_step {
            if !e.is_instance(py, &pdf_error) {
                return Err(e);
            }
            pdf_pages.clear();
            unsupported = g(py, "pdf_page_indices")?.call1((&payload,))?;
        }
        let page_count: usize = inventory.page_count;
        if candidates.len() < page_count && g(py, "_ink_provider")?.call0()?.is_truthy()? {
            let ink_step = (|| -> PyResult<()> {
                let snapshot = json_loads.call1((&base,))?;
                for rendered in slf.call_method1("_ink_inventory", (&snapshot,))?.try_iter()? {
                    let rendered = rendered?;
                    let index = rendered.get_item("index")?;
                    if !candidates.contains(&index)? && !pdf_pages.contains(&index)? && !unsupported.contains(&index)? {
                        let dark = rendered.call_method1("get", ("darkMode", false))?;
                        let item = g(py, "InkGeometryPreview")?.call1((&index, rendered.get_item("width")?, rendered.get_item("height")?,
                            snapshot.get_item("raw_sha256")?, dark))?;
                        candidates.set_item(index, item)?;
                    }
                }
                Ok(())
            })();
            if let Err(e) = ink_step {
                let tolerated = e.is_instance(py, &xarea::class(py, "samsung_notes.document", "DocumentError")?) || e.is_instance(py, &xarea::class(py, "samsung_notes.ink", "InkError")?)
                    || e.is_instance_of::<pyo3::exceptions::PyValueError>(py) || e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
                    || e.is_instance_of::<pyo3::exceptions::PyTypeError>(py);
                if !tolerated {
                    return Err(e);
                }
            }
        }
        for index in 0..page_count {
            let item = candidates.get_item(index)?;
            let page = PyDict::new(py);
            page.set_item("index", index)?;
            let none = py.None().into_bound(py);
            page.set_item("width", item.as_ref().map(|i| i.getattr("width")).transpose()?.unwrap_or_else(|| none.clone()))?;
            page.set_item("height", item.as_ref().map(|i| i.getattr("height")).transpose()?.unwrap_or_else(|| none.clone()))?;
            page.set_item("available", item.is_some())?;
            let special = item.as_ref().is_some_and(|i| is_pdf(i) || is_ink(i));
            let dark = match &item {
                Some(i) if special => i.getattr("is_dark_mode")?,
                _ => false.into_pyobject(py)?.to_owned().into_any(),
            };
            page.set_item("darkMode", dark)?;
            match &item {
                Some(i) if is_pdf(i) => {
                    let complete = i.getattr("complete")?;
                    let kind = if i.getattr("overlay")?.is_truthy()? { "pdf-with-notes" } else { "pdf-background" };
                    page.set_item("previewKind", kind)?;
                    page.set_item("previewNotice", if complete.is_truthy()? { none.clone() } else {
                        "PDF zemini gösteriliyor. Bu sayfadaki not ve çizimler henüz bu görüntüye eklenmiyor.".into_pyobject(py)?.into_any() })?;
                    page.set_item("complete", complete)?;
                    // Key order as in Python: previewKind, complete, previewNotice.
                    let notice = page.get_item("previewNotice")?;
                    page.del_item("previewNotice")?;
                    page.set_item("previewNotice", notice)?;
                }
                Some(i) if is_ink(i) => {
                    page.set_item("previewKind", "ink-reconstructed")?;
                    page.set_item("complete", true)?;
                    page.set_item("previewNotice", &none)?;
                }
                _ if pdf_pages.contains(index)? || unsupported.contains(index)? => {
                    page.set_item("complete", false)?;
                    page.set_item("previewNotice", "Bu sayfanın PDF yerleşimi henüz tam görüntülenemiyor.")?;
                }
                _ => {}
            }
            pages.append(page)?;
        }
        Ok(())
    }

    #[cfg(feature = "python")]
    #[allow(clippy::too_many_arguments)]
    fn process<'py>(slf: &Bound<'py, Self>, _key: &Bound<'py, PyAny>, identity: &Bound<'py, PyAny>, source: &Bound<'py, PyAny>,
                    candidate: &Bound<'py, PyAny>, path: &Bound<'py, PyAny>, session: &Bound<'py, PyAny>, local_pdf: &Bound<'py, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let manager: Py<PreviewManager> = slf.clone().unbind();
        let (identity_o, source_o, candidate_o, path_o, session_o) =
            (identity.clone().unbind(), source.clone().unbind(), candidate.clone().unbind(), path.clone().unbind(), session.clone().unbind());
        let no_local_pdf = local_pdf.is_none();
        let is_ink = is_ink(candidate);
        let guard = PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
            let py = args.py();
            let this = manager.bind(py);
            let sessions = this.getattr("engine")?.getattr("sessions")?;
            let changed = this.getattr("stop")?.call_method0("is_set")?.is_truthy()?
                || sessions.getattr("path")?.ne(session_o.bind(py))?
                || (sessions.getattr("offline")?.is_truthy()? && no_local_pdf && !is_ink)
                || this.call_method1("_source", (identity_o.bind(py),))?.ne(source_o.bind(py))?
                || this.call_method1("_path", (candidate_o.bind(py),))?.ne(path_o.bind(py))?;
            if changed {
                return Err(preview_error(py, "Not veya hesap değişti. Sayfayı yeniden açabilirsin."));
            }
            Ok(())
        })?.into_any();
        guard.call0()?;
        let engine = slf.getattr("engine")?;
        let (width, height) = (candidate.getattr("width")?, candidate.getattr("height")?);
        let size = PyTuple::new(py, [&width, &height])?;
        let guarded = kwargs(py, &[("guard", guard.clone())])?;
        let png = if is_ink {
            let render = py.import("samsung_notes.ink_render")?.getattr("render_ink_page")?;
            let base = engine.getattr("store")?.call_method1("raw", (identity,))?.get_item("base_document")?;
            let snapshot = py.import("json")?.getattr("loads")?.call1((base,))?;
            if snapshot.call_method1("get", ("raw_sha256",))?.ne(candidate.getattr("sha256")?)? {
                return Err(preview_error(py, "Not değişti. Sayfayı yeniden açabilirsiniz."));
            }
            render.call1((snapshot, candidate.getattr("page_index")?))?
        } else if is_pdf(candidate) {
            let pdf = if local_pdf.is_truthy()? {
                local_pdf.clone()
            } else {
                slf.call_method1("_fetch_pdf_refreshing", (identity, candidate, &guard))?
            };
            guard.call0()?;
            let mut png = g(py, "render_pdf_background")?.call1((pdf, candidate))?;
            let overlay = candidate.getattr("overlay")?;
            if !overlay.is_none() {
                guard.call0()?;
                let layer_path = slf.call_method1("_path", (&overlay,))?;
                let mut layer = read_cached(py, &layer_path, px::MAX_OUTPUT)?.map(|raw| PyBytes::new(py, &raw).into_any());
                if layer.as_ref().is_some_and(|l| !_png_valid(l, &width, &height)) {
                    layer = None;
                }
                let layer = match layer {
                    Some(l) => l,
                    None => {
                        let sessions = sessions(&engine)?;
                        if sessions.getattr("offline")?.is_truthy()? || !sessions.getattr("path")?.is_truthy()? {
                            return Err(preview_error(py, "Bu sayfanın çizim katmanı henüz çevrimdışı kullanılabilir değil."));
                        }
                        let raw = slf.getattr("fetcher")?.call((&overlay, engine.call_method0("_api")?), Some(&guarded))?;
                        guard.call0()?;
                        let layer = slf.getattr("decoder")?.call1((raw, &size))?;
                        slf.call_method1("_cache_png", (&layer_path, &layer, &width, &height, &guard))?;
                        layer
                    }
                };
                guard.call0()?;
                png = g(py, "compose_pdf_canvas")?.call1((png, layer, &width, &height))?;
            }
            png
        } else {
            let raw = slf.getattr("fetcher")?.call((candidate, engine.call_method0("_api")?), Some(&guarded))?;
            guard.call0()?;
            let mut png = slf.getattr("decoder")?.call1((raw, &size))?;
            if candidate.getattr("is_dark_mode")?.is_truthy()? {
                png = py.import("samsung_notes.page_image")?.getattr("light_from_dark")?.call1((png, &width, &height))?;
            }
            png
        };
        guard.call0()?;
        slf.call_method1("_cache_png", (path, png, &width, &height, &guard))?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PreviewError>()?;
    m.add("_Entry", crate::dataclasses::entry(m.py())?)?;
    m.add("InkGeometryPreview", crate::dataclasses::ink_geometry_preview(m.py())?)?;
    m.add_class::<PreviewManager>()?;
    m.add_function(wrap_pyfunction!(_url, m)?)?;
    m.add_function(wrap_pyfunction!(fetch_preview, m)?)?;
    m.add_function(wrap_pyfunction!(_png_valid, m)?)?;
    m.add_function(wrap_pyfunction!(_codec, m)?)?;
    m.add_function(wrap_pyfunction!(provider_available, m)?)?;
    m.add_function(wrap_pyfunction!(_ink_provider, m)?)?;
    m.add_function(wrap_pyfunction!(_payload_from_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(_inventory_from_snapshot, m)?)?;
    let _ = pdf_background::MAX_PDF_BYTES;
    rs_common::export(m, &["PreviewError", "_Entry", "InkGeometryPreview", "PreviewManager", "_url", "fetch_preview",
        "_png_valid", "_codec", "provider_available", "_ink_provider", "_payload_from_snapshot", "_inventory_from_snapshot"])
}
