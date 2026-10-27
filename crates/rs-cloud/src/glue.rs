//! PyO3 glue shared by the cloud submodules: exception classes, error
//! classification (Python `except (...)` clauses), file helpers raising the
//! same Python exceptions as the Python helpers, and Python-object helpers.
use crate::pyjson::{self, Value};
use crate::util::{self, LoadError, OnceError};
#[cfg(feature = "python")]
use pyo3::exceptions::{PyException, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple};
use std::path::{Path, PathBuf};

// ------------------------------------------------------------ exceptions

#[cfg(feature = "python")]
#[pyclass(extends=PyException, subclass, module = "samsung_notes.cloud_api", name = "APIError")]
pub struct APIError;

#[cfg(feature = "python")]
fn init_api_error(slf: &Bound<'_, PyAny>, code: &Bound<'_, PyAny>, message: &Bound<'_, PyAny>, ambiguous: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = slf.py();
    py.get_type::<PyException>().getattr("__init__")?.call1((slf, message))?;
    slf.setattr("code", code)?;
    slf.setattr("message", message)?;
    slf.setattr("ambiguous", ambiguous)?;
    Ok(())
}

#[cfg(feature = "python")]
#[pymethods]
impl APIError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        APIError
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (code, message, *, ambiguous = None))]
    fn __init__(slf: &Bound<'_, Self>, code: &Bound<'_, PyAny>, message: &Bound<'_, PyAny>, ambiguous: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let false_ = pyo3::types::PyBool::new(py, false).to_owned().into_any();
        init_api_error(slf.as_any(), code, message, ambiguous.unwrap_or(&false_))
    }
}

pub const CONFLICT_MESSAGE: &str = "Not başka bir cihazda değişti; yerel metin korunuyor.";
pub const UNSUPPORTED_MESSAGE: &str = "Bu not henüz güvenle düzenlenemiyor.";

#[cfg(feature = "python")]
#[pyclass(extends=APIError, subclass, module = "samsung_notes.cloud_api", name = "ConflictError")]
pub struct ConflictError;

#[cfg(feature = "python")]
#[pymethods]
impl ConflictError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyClassInitializer<Self> {
        PyClassInitializer::from(APIError).add_subclass(ConflictError)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (message = None))]
    fn __init__(slf: &Bound<'_, Self>, message: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let default = PyString::new(py, CONFLICT_MESSAGE).into_any();
        let false_ = pyo3::types::PyBool::new(py, false).to_owned().into_any();
        init_api_error(slf.as_any(), &PyString::new(py, "conflict").into_any(), message.unwrap_or(&default), &false_)
    }
}

#[cfg(feature = "python")]
#[pyclass(extends=APIError, subclass, module = "samsung_notes.cloud_api", name = "UnsupportedDocumentError")]
pub struct UnsupportedDocumentError;

#[cfg(feature = "python")]
#[pymethods]
impl UnsupportedDocumentError {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyClassInitializer<Self> {
        PyClassInitializer::from(APIError).add_subclass(UnsupportedDocumentError)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (message = None))]
    fn __init__(slf: &Bound<'_, Self>, message: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let default = PyString::new(py, UNSUPPORTED_MESSAGE).into_any();
        let false_ = pyo3::types::PyBool::new(py, false).to_owned().into_any();
        init_api_error(slf.as_any(), &PyString::new(py, "unsupported_document").into_any(), message.unwrap_or(&default), &false_)
    }
}

#[cfg(feature = "python")]
fn instantiate<'py, A: pyo3::call::PyCallArgs<'py>>(cls: Bound<'py, pyo3::types::PyType>, args: A, kwargs: Option<&Bound<'py, PyDict>>) -> PyErr {
    match cls.call(args, kwargs) {
        Ok(inst) => PyErr::from_value(inst),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
pub fn api_error(py: Python<'_>, code: &str, message: &str) -> PyErr {
    instantiate(py.get_type::<APIError>(), (code, message), None)
}

#[cfg(feature = "python")]
pub fn api_error_ambiguous(py: Python<'_>, code: &str, message: &str, ambiguous: bool) -> PyErr {
    let kw = PyDict::new(py);
    if kw.set_item("ambiguous", ambiguous).is_err() {
        return api_error(py, code, message);
    }
    instantiate(py.get_type::<APIError>(), (code, message), Some(&kw))
}

#[cfg(feature = "python")]
pub fn uncertain(py: Python<'_>, code: &str, message: &str) -> PyErr {
    api_error_ambiguous(py, code, message, true)
}

#[cfg(feature = "python")]
pub fn conflict(py: Python<'_>, message: Option<&str>) -> PyErr {
    match message {
        Some(m) => instantiate(py.get_type::<ConflictError>(), (m,), None),
        None => instantiate(py.get_type::<ConflictError>(), (), None),
    }
}

#[cfg(feature = "python")]
pub fn unsupported(py: Python<'_>, message: Option<&str>) -> PyErr {
    match message {
        Some(m) => instantiate(py.get_type::<UnsupportedDocumentError>(), (m,), None),
        None => instantiate(py.get_type::<UnsupportedDocumentError>(), (), None),
    }
}

#[cfg(feature = "python")]
/// `UnsupportedDocumentError(str(error))`
pub fn unsupported_from(py: Python<'_>, error: &PyErr) -> PyErr {
    match error.value(py).str() {
        Ok(s) => unsupported(py, Some(&s.to_string_lossy())),
        Err(e) => e,
    }
}

// ------------------------------------------------------------ classification

#[derive(Clone, Copy)]
pub enum K {
    OS,
    Value,
    Key,
    Type,
    Attr,
    Api,
    Parse,
    Doc,
    Ink,
    Unicode,
    Overflow,
    Subprocess,
    StopIter,
}

#[cfg(feature = "python")]
pub fn class_of<'py>(py: Python<'py>, k: K) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(feature = "python")]
    use pyo3::exceptions::*;
    Ok(match k {
        K::OS => py.get_type::<PyOSError>().into_any(),
        K::Value => py.get_type::<PyValueError>().into_any(),
        K::Key => py.get_type::<PyKeyError>().into_any(),
        K::Type => py.get_type::<PyTypeError>().into_any(),
        K::Attr => py.get_type::<PyAttributeError>().into_any(),
        K::Api => py.get_type::<APIError>().into_any(),
        K::Parse => py.import("xml.etree.ElementTree")?.getattr("ParseError")?,
        K::Doc => document_error_class(py)?,
        K::Ink => crate::areas::live(py, "samsung_notes.ink", "InkError")?,
        K::Unicode => py.get_type::<PyUnicodeError>().into_any(),
        K::Overflow => py.get_type::<PyOverflowError>().into_any(),
        K::Subprocess => py.import("subprocess")?.getattr("SubprocessError")?,
        K::StopIter => py.get_type::<PyStopIteration>().into_any(),
    })
}

#[cfg(feature = "python")]
pub fn document_error_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    crate::areas::live(py, "samsung_notes.document", "DocumentError")
}

#[cfg(feature = "python")]
pub fn document_error(py: Python<'_>, message: &str) -> PyErr {
    rs_doc::pyapi::document_error(py, message)
}

#[cfg(feature = "python")]
/// Whether `except (kinds...)` would catch this error.
pub fn caught(py: Python<'_>, e: &PyErr, kinds: &[K]) -> bool {
    let value = e.value(py);
    kinds.iter().any(|k| class_of(py, *k).map(|c| value.is_instance(&c).unwrap_or(false)).unwrap_or(false))
}

#[cfg(feature = "python")]
/// Run a Python-style `try:` body; errors matching `kinds` become `to()`.
pub fn try_map<T>(py: Python<'_>, r: PyResult<T>, kinds: &[K], to: impl FnOnce() -> PyErr) -> PyResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) if caught(py, &e, kinds) => Err(to()),
        Err(e) => Err(e),
    }
}

#[cfg(feature = "python")]
pub fn value_error() -> PyErr {
    PyValueError::new_err(())
}

// ------------------------------------------------------------ files

#[cfg(feature = "python")]
pub fn fspath(o: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
    o.extract::<PathBuf>()
}

#[cfg(feature = "python")]
pub fn py_path<'py>(py: Python<'py>, p: &Path) -> PyResult<Bound<'py, PyAny>> {
    py.import("pathlib")?.getattr("Path")?.call1((p.to_path_buf(),))
}

#[cfg(feature = "python")]
pub fn load_error(py: Python<'_>, e: LoadError) -> PyErr {
    match e {
        LoadError::Io(io) => io.into(),
        LoadError::TooLarge => PyValueError::new_err("private input exceeds size limit"),
        LoadError::Json(d, raw) => pyjson::decode_error(py, &d, &raw),
    }
}

#[cfg(feature = "python")]
pub fn load_value(py: Python<'_>, path: &Path, limit: usize) -> PyResult<Value> {
    util::load_json(path, limit).map_err(|e| load_error(py, e))
}

#[cfg(feature = "python")]
pub fn load_json<'py>(py: Python<'py>, path: &Path, limit: usize) -> PyResult<Bound<'py, PyAny>> {
    pyjson::to_py(py, &load_value(py, path, limit)?)
}

pub const DEFAULT_LIMIT: usize = 128 * 1024;

#[cfg(feature = "python")]
pub fn save_json(path: &Path, value: &Bound<'_, PyAny>) -> PyResult<()> {
    let v = pyjson::from_py(value)?;
    Ok(util::save_json(path, &v)?)
}

#[cfg(feature = "python")]
pub fn save_value(path: &Path, value: &Value) -> PyResult<()> {
    Ok(util::save_json(path, value)?)
}

#[cfg(feature = "python")]
pub fn private_write(path: &Path, data: &[u8]) -> PyResult<()> {
    Ok(util::private_write(path, data)?)
}

#[cfg(feature = "python")]
/// cloud_api._save_once
pub fn save_once(py: Python<'_>, path: &Path, data: &[u8]) -> PyResult<()> {
    util::save_once(path, data).map_err(|e| match e {
        OnceError::Io(io) => io.into(),
        OnceError::Mismatch => api_error(py, "operation_mismatch", "Hazırlanmış işlemin içeriği değişti."),
    })
}

#[cfg(feature = "python")]
/// cloud_api._json_once
pub fn json_once(py: Python<'_>, path: &Path, value: &Bound<'_, PyAny>) -> PyResult<()> {
    let v = pyjson::from_py(value)?;
    save_once(py, path, &util::json_bytes(&v))
}

pub fn exists(p: &Path) -> bool {
    p.exists()
}

// ------------------------------------------------------------ objects

#[cfg(feature = "python")]
pub fn dict<'py>(py: Python<'py>, items: &[(&str, Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in items {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

#[cfg(feature = "python")]
pub fn s<'py>(py: Python<'py>, v: &str) -> Bound<'py, PyAny> {
    PyString::new(py, v).into_any()
}

#[cfg(feature = "python")]
pub fn b<'py>(py: Python<'py>, v: &[u8]) -> Bound<'py, PyAny> {
    PyBytes::new(py, v).into_any()
}

#[cfg(feature = "python")]
pub fn int<'py>(py: Python<'py>, v: i64) -> Bound<'py, PyAny> {
    v.into_pyobject(py).unwrap().into_any()
}

#[cfg(feature = "python")]
pub fn none(py: Python<'_>) -> Bound<'_, PyAny> {
    py.None().into_bound(py)
}

#[cfg(feature = "python")]
pub fn boolean(py: Python<'_>, v: bool) -> Bound<'_, PyAny> {
    pyo3::types::PyBool::new(py, v).to_owned().into_any()
}

#[cfg(feature = "python")]
/// `isinstance(v, str) and 0 < len(v) <= limit and no control characters`
pub fn is_string(v: &Bound<'_, PyAny>, limit: usize) -> bool {
    match v.cast::<PyString>() {
        Ok(s) => match s.to_str() {
            Ok(t) => util::is_safe_string(t, limit),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

#[cfg(feature = "python")]
/// exchange_account_code.safe_text
pub fn safe_text(v: &Bound<'_, PyAny>, maximum: usize) -> bool {
    match v.cast::<PyString>() {
        Ok(s) => match s.to_str() {
            Ok(t) => {
                let n = t.chars().count();
                0 < n && n <= maximum && t.chars().all(|c| 32 < c as u32 && (c as u32) < 127)
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}

#[cfg(feature = "python")]
/// `str` attribute or Python-identical TypeError.
pub fn str_of(v: &Bound<'_, PyAny>) -> PyResult<String> {
    v.cast::<PyString>().map_err(|_| PyTypeError::new_err(format!("expected str, not {}", v.get_type().name().map(|n| n.to_string()).unwrap_or_default())))?.to_str().map(str::to_string)
}

#[cfg(feature = "python")]
pub fn opt_str(v: &Bound<'_, PyAny>) -> Option<String> {
    v.cast::<PyString>().ok().and_then(|s| s.to_str().ok().map(str::to_string))
}

#[cfg(feature = "python")]
/// Python `int(x * 1000)` for the clock value.
pub fn millis(now: &Bound<'_, PyAny>) -> PyResult<i64> {
    let py = now.py();
    py.import("builtins")?.getattr("int")?.call1((now.mul(1000)?,))?.extract()
}

#[cfg(feature = "python")]
/// Status values compared with ints (`status != 201`), None/other -> None.
pub fn status_code(v: &Bound<'_, PyAny>) -> Option<i64> {
    if v.is_instance_of::<pyo3::types::PyBool>() {
        return v.extract::<bool>().ok().map(|b| b as i64);
    }
    v.extract::<i64>().ok()
}

#[cfg(feature = "python")]
/// Strings of an iterable (None stays None; other objects never match).
pub fn opt_strings(v: &Bound<'_, PyAny>) -> PyResult<Vec<Option<String>>> {
    let mut out = Vec::new();
    for item in v.try_iter()? {
        let item = item?;
        out.push(if item.is_none() { None } else { Some(opt_str(&item).unwrap_or_else(|| format!("\u{0}non-str:{}", item.as_ptr() as usize))) });
    }
    Ok(out)
}

#[cfg(feature = "python")]
pub fn request<'py>(py: Python<'py>, url: &str, data: Option<&[u8]>, method: Option<&str>, headers: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    if let Some(d) = data {
        kw.set_item("data", PyBytes::new(py, d))?;
    }
    if let Some(m) = method {
        kw.set_item("method", m)?;
    }
    kw.set_item("headers", headers)?;
    py.import("urllib.request")?.getattr("Request")?.call((url,), Some(&kw))
}

#[cfg(feature = "python")]
pub fn sha256_obj(v: &Bound<'_, PyAny>) -> PyResult<String> {
    let data: Vec<u8> = v.cast::<PyBytes>().map_err(|_| PyTypeError::new_err("object supporting the buffer API required"))?.as_bytes().to_vec();
    Ok(util::sha256_hex(&data))
}

#[cfg(feature = "python")]
pub fn bytes_of(v: &Bound<'_, PyAny>) -> Option<Vec<u8>> {
    v.cast::<PyBytes>().ok().map(|b| b.as_bytes().to_vec())
}

#[cfg(feature = "python")]
/// `str.encode()` of a str attribute (UTF-8; surrogates raise like Python).
pub fn utf8_of(v: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    Ok(v.call_method0("encode")?.cast::<PyBytes>()?.as_bytes().to_vec())
}
