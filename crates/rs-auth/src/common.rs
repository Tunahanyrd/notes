//! Small PyO3 helpers shared by the auth submodules. Pure logic lives in Rust;
//! stdlib facilities (re, hmac, hashlib, urllib.parse) are called through
//! Python so the ported functions stay byte-for-byte faithful.
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyDict, PyInt, PyString, PyTuple};
use std::path::Path;

#[cfg(feature = "python")]
/// `re.fullmatch(pattern, text)` is truthy.
pub fn re_fullmatch(py: Python<'_>, pattern: &str, text: &str) -> PyResult<bool> {
    let m = py.import("re")?.call_method1("fullmatch", (pattern, text))?;
    Ok(m.is_truthy()?)
}

#[cfg(feature = "python")]
/// `hmac.compare_digest(a, b)` for two str values.
pub fn compare_digest(py: Python<'_>, a: &str, b: &str) -> PyResult<bool> {
    py.import("hmac")?.call_method1("compare_digest", (a, b))?.extract()
}

#[cfg(feature = "python")]
/// Python `type(value) is int` (bool is a distinct type and is rejected).
pub fn is_strict_int(v: &Bound<'_, PyAny>) -> bool {
    v.is_instance_of::<PyInt>() && !v.is_instance_of::<PyBool>()
}

#[cfg(feature = "python")]
/// Python `type(value) in (int, float)` (bool rejected like `type(x) in (...)`).
pub fn is_strict_number(v: &Bound<'_, PyAny>) -> bool {
    (v.is_instance_of::<PyInt>() || v.is_instance_of::<pyo3::types::PyFloat>())
        && !v.is_instance_of::<PyBool>()
}

#[cfg(feature = "python")]
/// `dict.get(key)` → Bound (None when absent).
pub fn get<'py>(o: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    o.call_method1("get", (key,))
}

#[cfg(feature = "python")]
/// `dict.get(key, {})`.
pub fn get_or_empty<'py>(o: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    o.call_method1("get", (key, PyDict::new(o.py())))
}

#[cfg(feature = "python")]
pub fn value_error(message: &str) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(message.to_string())
}

#[cfg(feature = "python")]
/// A Python `str` object.
pub fn s<'py>(py: Python<'py>, v: &str) -> Bound<'py, PyAny> {
    PyString::new(py, v).into_any()
}

#[cfg(feature = "python")]
/// Build a `urllib.request.Request`.
pub fn request<'py>(
    py: Python<'py>,
    url: &str,
    data: Option<&[u8]>,
    method: &str,
    headers: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    if let Some(d) = data {
        kw.set_item("data", PyBytes::new(py, d))?;
    }
    kw.set_item("method", method)?;
    kw.set_item("headers", headers)?;
    py.import("urllib.request")?.getattr("Request")?.call((url,), Some(&kw))
}

/// `''.join(secrets.choice(alphabet) for _ in range(n))`.
pub fn secrets_choice(n: usize) -> String {
    rs_cloud::util::random_choice("abcdefghijklmnopqrstuvwxyz", n)
}

#[cfg(feature = "python")]
/// A str Bound, or None → raises the given message when not a str.
pub fn require_str(v: &Bound<'_, PyAny>, message: &str) -> PyResult<String> {
    match v.cast::<PyString>() {
        Ok(s) => Ok(s.to_str()?.to_string()),
        Err(_) => Err(value_error(message)),
    }
}

#[cfg(feature = "python")]
/// `exchange_account_code.safe_text(value, maximum=8192)`.
pub fn safe_text(v: &Bound<'_, PyAny>, maximum: usize) -> bool {
    rs_cloud::glue::safe_text(v, maximum)
}

/// `cloud_api._string`-style control-character check for an already-str value.
pub fn no_controls(s: &str) -> bool {
    !s.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
}

/// os.open(path, O_WRONLY|O_CREAT|O_EXCL, 0o600); write; flush; fsync.
/// Returns an `AlreadyExists` io::Error (→ Python FileExistsError) if present.
pub fn excl_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()
}

/// Read a file as bytes, bounded to `limit + 1` like the Python readers.
pub fn read_bounded(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut raw = Vec::new();
    f.take(limit as u64 + 1).read_to_end(&mut raw)?;
    Ok(raw)
}

#[cfg(feature = "python")]
/// `json.loads(bytes)` → Bound, raising json.JSONDecodeError like Python.
pub fn json_loads<'py>(py: Python<'py>, raw: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    match rs_cloud::pyjson::loads(raw) {
        Ok(v) => rs_cloud::pyjson::to_py(py, &v),
        Err(e) => Err(rs_cloud::pyjson::decode_error(py, &e, raw)),
    }
}

#[cfg(feature = "python")]
/// Build `urllib.request.build_opener(NoRedirect(), HTTPSHandler(context=
/// ssl.create_default_context()))`. `build_opener` is imported fresh so the
/// test seam `patch('<module>.urllib.request.build_opener', ...)` is honored and
/// no external request is made in tests.
pub fn build_opener<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    let ur = py.import("urllib.request")?;
    let ctx = py.import("ssl")?.call_method0("create_default_context")?;
    let noredirect = py.import("probe_account_entrypoint")?.getattr("NoRedirect")?.call0()?;
    let https_kw = PyDict::new(py);
    https_kw.set_item("context", ctx)?;
    let https = ur.getattr("HTTPSHandler")?.call((), Some(&https_kw))?;
    ur.getattr("build_opener")?.call1((noredirect, https))
}

#[cfg(feature = "python")]
/// `opener.open(request, timeout=t)`; an `HTTPError` is returned as the response
/// object (like `except HTTPError as error: response = error`).
pub fn opener_open<'py>(
    py: Python<'py>,
    opener: &Bound<'py, PyAny>,
    request: &Bound<'py, PyAny>,
    timeout: i64,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("timeout", timeout)?;
    match opener.call_method("open", (request,), Some(&kw)) {
        Ok(r) => Ok(r),
        Err(e) => {
            let http_error = py.import("urllib.error")?.getattr("HTTPError")?;
            if e.value(py).is_instance(&http_error)? {
                Ok(e.value(py).clone().into_any())
            } else {
                Err(e)
            }
        }
    }
}

#[cfg(feature = "python")]
/// True when the error is one of `(OSError, ValueError, EOFError)` (the usual
/// transport/response `except` tuple; UnicodeError/URLError/HTTPError are
/// subclasses of these).
pub fn is_transport_error(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<PyValueError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyEOFError>(py)
}

#[cfg(feature = "python")]
/// True when the error is one of `(OSError, ValueError, UnicodeError)`.
pub fn is_os_value_unicode(py: Python<'_>, e: &PyErr) -> bool {
    e.is_instance_of::<pyo3::exceptions::PyOSError>(py)
        || e.is_instance_of::<PyValueError>(py)
        || e.is_instance_of::<pyo3::exceptions::PyUnicodeError>(py)
}

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;

#[cfg(feature = "python")]
pub fn tuple3<'py>(
    py: Python<'py>,
    a: Bound<'py, PyAny>,
    b: Bound<'py, PyAny>,
    c: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, [a, b, c])
}
