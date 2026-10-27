//! PyO3 helpers shared by the doc bindings and usable by other area crates.
//!
//! NOTE for other crates linking `rs-doc` as an rlib: pyclasses and exception
//! types are per-extension objects. Never construct `rs_doc` pyclasses or raise
//! the static exception types from another cdylib; use
//! [`document_error`] / [`payload_error`] (they import the live classes) and
//! call the Python-level `samsung_notes.document.DocumentSnapshot` etc.
#[cfg(feature = "python")]
use pyo3::exceptions::{PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::sync::PyOnceLock;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PyModule, PyString, PyTuple};

use crate::error::Error;
use crate::mime::{self, FastMatch, Headers, Limits};
use crate::xml::Element;

#[cfg(feature = "python")]
pyo3::create_exception!(notes_payload, PayloadError, PyValueError, "Messages are static and never include private payload values.");
#[cfg(feature = "python")]
pyo3::create_exception!(document, DocumentError, PyValueError);

pub const PAYLOAD_MODULE: &str = "tools.notes_payload";
pub const DOCUMENT_MODULE: &str = "samsung_notes.document";
pub const WRITER_MODULE: &str = "tools.notes_writer";

#[cfg(feature = "python")]
/// Live `PayloadError` (whatever `tools.notes_payload.PayloadError` currently is).
pub fn payload_error(py: Python<'_>, msg: &str) -> PyErr {
    match py.import(PAYLOAD_MODULE).and_then(|m| m.getattr("PayloadError")) {
        Ok(cls) => PyErr::from_value(cls.call1((msg,)).unwrap_or_else(|e| e.into_value(py).into_bound(py).into_any())),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
/// Live `DocumentError` (whatever `samsung_notes.document.DocumentError` currently is).
pub fn document_error(py: Python<'_>, msg: &str) -> PyErr {
    match py.import(DOCUMENT_MODULE).and_then(|m| m.getattr("DocumentError")) {
        Ok(cls) => PyErr::from_value(cls.call1((msg,)).unwrap_or_else(|e| e.into_value(py).into_bound(py).into_any())),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
/// Map a core error onto the Python exception the original module raised
/// (static exception types of this extension).
pub fn to_pyerr(py: Python<'_>, err: Error) -> PyErr {
    match err {
        Error::Payload(m) => PayloadError::new_err(m),
        Error::Document(m) => DocumentError::new_err(m.into_owned()),
        Error::Value(m) => PyValueError::new_err(m.into_owned()),
        Error::Type(m) => PyTypeError::new_err(m.into_owned()),
        Error::XmlParse(m) => match py.import("xml.etree.ElementTree").and_then(|et| et.getattr("ParseError")) {
            Ok(cls) => match cls.call1((m,)) {
                Ok(v) => PyErr::from_value(v),
                Err(e) => e,
            },
            Err(e) => e,
        },
        Error::Io(e) => PyErr::from(e),
        Error::External(b) => match b.downcast::<PyErr>() {
            Ok(e) => *e,
            Err(_) => PyValueError::new_err("external error"),
        },
    }
}

pub trait IntoPy<T> {
    #[cfg(feature = "python")]
    fn py(self, py: Python<'_>) -> PyResult<T>;
}
impl<T> IntoPy<T> for crate::Result<T> {
    #[cfg(feature = "python")]
    fn py(self, py: Python<'_>) -> PyResult<T> {
        self.map_err(|e| to_pyerr(py, e))
    }
}

#[cfg(feature = "python")]
fn get_usize(m: &Bound<'_, PyModule>, name: &str, default: usize) -> usize {
    m.getattr(name).ok().and_then(|v| v.extract::<i64>().ok()).map_or(default, |v| v.max(0) as usize)
}
#[cfg(feature = "python")]
fn get_i64(m: &Bound<'_, PyModule>, name: &str, default: i64) -> i64 {
    m.getattr(name).ok().and_then(|v| v.extract::<i64>().ok()).unwrap_or(default)
}

#[cfg(feature = "python")]
/// Loaded module objects of `tools.notes_payload` (also importable as the
/// top-level `notes_payload` from tools/). Patches may target either.
pub fn payload_modules(py: Python<'_>) -> PyResult<Vec<Bound<'_, PyModule>>> {
    let modules = py.import("sys")?.getattr("modules")?;
    let mut out: Vec<Bound<'_, PyModule>> = Vec::new();
    for name in [PAYLOAD_MODULE, "notes_payload"] {
        if let Ok(m) = modules.get_item(name) {
            if let Ok(m) = m.cast_into::<PyModule>() {
                if !out.iter().any(|o| o.is(&m)) {
                    out.push(m);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(py.import(PAYLOAD_MODULE)?);
    }
    Ok(out)
}

#[cfg(feature = "python")]
fn limit_usize(ms: &[Bound<'_, PyModule>], name: &str, default: usize) -> usize {
    ms.iter().map(|m| get_usize(m, name, default)).find(|v| *v != default).unwrap_or(default)
}
#[cfg(feature = "python")]
fn limit_i64(ms: &[Bound<'_, PyModule>], name: &str, default: i64) -> i64 {
    ms.iter().map(|m| get_i64(m, name, default)).find(|v| *v != default).unwrap_or(default)
}

#[cfg(feature = "python")]
/// Limits as currently set on `tools.notes_payload` / `notes_payload` (tests patch them).
pub fn payload_limits(py: Python<'_>) -> PyResult<Limits> {
    let d = Limits::default();
    let m = payload_modules(py)?;
    Ok(Limits {
        max_body_bytes: limit_usize(&m, "MAX_BODY_BYTES", d.max_body_bytes),
        max_parts: limit_i64(&m, "MAX_PARTS", d.max_parts),
        max_readonly_parts: limit_i64(&m, "MAX_READONLY_PARTS", d.max_readonly_parts),
        max_header_bytes: limit_usize(&m, "MAX_HEADER_BYTES", d.max_header_bytes),
        max_xml_bytes: limit_usize(&m, "MAX_XML_BYTES", d.max_xml_bytes),
        max_xml_nodes: limit_i64(&m, "MAX_XML_NODES", d.max_xml_nodes),
        max_xml_depth: limit_i64(&m, "MAX_XML_DEPTH", d.max_xml_depth),
        max_text_bytes: limit_usize(&m, "MAX_TEXT_BYTES", d.max_text_bytes),
        max_field_bytes: limit_usize(&m, "MAX_FIELD_BYTES", d.max_field_bytes),
        max_resources: limit_usize(&m, "MAX_RESOURCES", d.max_resources),
    })
}

#[cfg(feature = "python")]
pub fn doc_limits(py: Python<'_>) -> PyResult<crate::doc::DocLimits> {
    let payload = payload_limits(py)?;
    let m = py.import(DOCUMENT_MODULE)?;
    Ok(crate::doc::DocLimits {
        max_body_bytes: get_usize(&m, "MAX_BODY_BYTES", payload.max_body_bytes),
        max_parts: get_i64(&m, "MAX_PARTS", payload.max_parts),
        max_readonly_parts: get_i64(&m, "MAX_READONLY_PARTS", payload.max_readonly_parts),
        max_text: get_usize(&m, "MAX_TEXT", crate::doc::MAX_TEXT),
        payload,
    })
}

// ------------------------------------------------------ builtin identities

#[cfg(feature = "python")]
pub(crate) static BUILTIN_PARTS: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
#[cfg(feature = "python")]
pub(crate) static BUILTIN_HEADERS: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
#[cfg(feature = "python")]
pub(crate) static BUILTIN_FAST: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
#[cfg(feature = "python")]
pub(crate) static BUILTIN_PRIVATE_WRITE: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

#[cfg(feature = "python")]
/// The module attribute if it is no longer this extension's builtin (patched).
pub(crate) fn patched<'py>(
    module: &Bound<'py, PyModule>,
    name: &str,
    builtin: &PyOnceLock<Py<PyAny>>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let current = match module.getattr(name) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    match builtin.get(module.py()) {
        Some(b) if current.is(b.bind(module.py())) => Ok(None),
        // Python implementation still installed (NOTES_IMPL=python never loads us)
        _ => Ok(Some(current)),
    }
}

#[cfg(feature = "python")]
/// First patched attribute among all loaded payload module aliases.
pub(crate) fn patched_any<'py>(py: Python<'py>, name: &str, builtin: &PyOnceLock<Py<PyAny>>) -> PyResult<Option<Bound<'py, PyAny>>> {
    for m in payload_modules(py)? {
        if let Some(p) = patched(&m, name, builtin)? {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

#[cfg(feature = "python")]
/// Header object returned by the exported `_headers` (email.message subset).
#[pyclass(module = "tools.notes_payload", name = "_Headers", frozen)]
pub struct PyHeaders {
    pub inner: Headers,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyHeaders {
    fn get_content_type(&self) -> String {
        self.inner.content_type()
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (failobj=None))]
    fn get_boundary(&self, py: Python<'_>, failobj: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self.inner.boundary().py(py)? {
            Some(b) => Ok(PyString::new(py, &b).into_any().unbind()),
            None => Ok(failobj.unwrap_or_else(|| py.None())),
        }
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (param, failobj=None, header="content-type", unquote=true))]
    fn get_param(&self, py: Python<'_>, param: &str, failobj: Option<Py<PyAny>>, header: &str, unquote: bool) -> PyResult<Py<PyAny>> {
        let _ = unquote;
        match self.inner.param(header, param) {
            Ok(Some(v)) => Ok(PyString::new(py, &v).into_any().unbind()),
            Ok(None) => Ok(failobj.unwrap_or_else(|| py.None())),
            Err(()) => Err(PyValueError::new_err("RFC 2231 parameters are not supported")),
        }
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (failobj=None))]
    fn get_filename(&self, py: Python<'_>, failobj: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self.inner.filename() {
            Ok(Some(v)) => Ok(PyString::new(py, &v).into_any().unbind()),
            Ok(None) => Ok(failobj.unwrap_or_else(|| py.None())),
            Err(()) => Err(PyValueError::new_err("RFC 2231 parameters are not supported")),
        }
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (name, failobj=None))]
    fn get(&self, py: Python<'_>, name: &str, failobj: Option<Py<PyAny>>) -> Py<PyAny> {
        let v = match name.to_ascii_lowercase().as_str() {
            "content-type" => self.inner.content_type.as_deref(),
            "content-disposition" => self.inner.disposition.as_deref(),
            "content-transfer-encoding" => self.inner.transfer_encoding.as_deref(),
            _ => None,
        };
        match v {
            Some(v) => PyString::new(py, v).into_any().unbind(),
            None => failobj.unwrap_or_else(|| py.None()),
        }
    }
    #[cfg(feature = "python")]
    fn __getitem__(&self, py: Python<'_>, name: &str) -> Py<PyAny> {
        self.get(py, name, None)
    }
    #[cfg(feature = "python")]
    #[pyo3(signature = (name, failobj=None))]
    fn get_all(&self, py: Python<'_>, name: &str, failobj: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        let v = self.get(py, name, None);
        if v.is_none(py) {
            return Ok(failobj.unwrap_or_else(|| py.None()));
        }
        Ok(PyList::new(py, [v])?.into_any().unbind())
    }
    #[cfg(feature = "python")]
    #[getter]
    fn defects(&self, py: Python<'_>) -> Py<PyAny> {
        PyList::empty(py).into_any().unbind()
    }
}

#[cfg(feature = "python")]
/// Hooks routing through patched `_headers` / `_SAMSUNG_PART_HEADERS`.
pub(crate) struct PyHooks<'py> {
    headers: Option<Bound<'py, PyAny>>,
    fast: Option<Bound<'py, PyAny>>,
}

#[cfg(feature = "python")]
impl<'py> PyHooks<'py> {
    #[cfg(feature = "python")]
    pub fn new(py: Python<'py>) -> PyResult<Self> {
        Ok(PyHooks {
            headers: patched_any(py, "_headers", &BUILTIN_HEADERS)?,
            fast: patched_any(py, "_SAMSUNG_PART_HEADERS", &BUILTIN_FAST)?,
        })
    }
}

#[cfg(feature = "python")]
fn external(e: PyErr) -> Error {
    Error::External(Box::new(e))
}

#[cfg(feature = "python")]
impl mime::Hooks for PyHooks<'_> {
    #[cfg(feature = "python")]
    fn headers(&mut self, raw: &[u8], limits: &Limits) -> crate::Result<Headers> {
        match &self.headers {
            None => mime::parse_headers(raw, limits),
            Some(f) => {
                let py = f.py();
                let out = f.call1((PyBytes::new(py, raw),)).map_err(external)?;
                let h = out
                    .cast::<PyHeaders>()
                    .map_err(|_| Error::Type("patched _headers must return the Rust header object".into()))?;
                Ok(h.get().inner.clone())
            }
        }
    }
    #[cfg(feature = "python")]
    fn fast_match(&mut self, raw: &[u8]) -> crate::Result<Option<FastMatch>> {
        match &self.fast {
            None => Ok(mime::samsung_fast_match(raw)),
            Some(pattern) => {
                let py = pattern.py();
                let m = pattern.call_method1("fullmatch", (PyBytes::new(py, raw),)).map_err(external)?;
                if m.is_none() {
                    return Ok(None);
                }
                let groups = m.call_method0("groups").map_err(external)?;
                let (name, filename, kind): (Vec<u8>, Option<Vec<u8>>, Vec<u8>) = groups.extract().map_err(external)?;
                let s = |v: Vec<u8>| String::from_utf8_lossy(&v).into_owned();
                Ok(Some(FastMatch { name: s(name), filename: filename.map(s), content_type: s(kind) }))
            }
        }
    }
}

#[cfg(feature = "python")]
/// `dataclasses` field table so `dataclasses.replace/asdict/fields` work.
pub fn dataclass_fields(py: Python<'_>, names: &[&str], defaults: &[(&str, Py<PyAny>)]) -> PyResult<Py<PyDict>> {
    let dc = py.import("dataclasses")?;
    let field = dc.getattr("field")?;
    let kind = dc.getattr("_FIELD")?;
    let out = PyDict::new(py);
    for name in names {
        let f = match defaults.iter().find(|(n, _)| n == name) {
            Some((_, v)) => {
                let kw = PyDict::new(py);
                kw.set_item("default", v.bind(py))?;
                field.call((), Some(&kw))?
            }
            None => field.call0()?,
        };
        f.setattr("name", *name)?;
        f.setattr("type", "object")?;
        f.setattr("_field_type", &kind)?;
        out.set_item(*name, f)?;
    }
    Ok(out.unbind())
}

#[cfg(feature = "python")]
/// Dataclass-style `==` over field tuples (only for exactly the same class).
pub fn fields_eq(py: Python<'_>, a_type: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>, a: Vec<Py<PyAny>>, getb: impl Fn() -> PyResult<Vec<Py<PyAny>>>) -> PyResult<Py<PyAny>> {
    if !b.get_type().is(a_type) {
        return Ok(py.NotImplemented());
    }
    let ta = PyTuple::new(py, a)?;
    let tb = PyTuple::new(py, getb()?)?;
    Ok(ta.eq(tb)?.into_pyobject(py)?.to_owned().into_any().unbind())
}

// ------------------------------------------------------- ElementTree bridge

#[cfg(feature = "python")]
/// Convert an `xml.etree.ElementTree.Element` into the Rust tree. Returns
/// `None` for anything the fast serializer does not model (comments, PIs,
/// QName objects, non-str values) so callers can fall back to `ET.tostring`.
pub fn element_from_py(obj: &Bound<'_, PyAny>) -> PyResult<Option<Element>> {
    #[cfg(feature = "python")]
    fn one(obj: &Bound<'_, PyAny>) -> PyResult<Option<Element>> {
        let tag = obj.getattr("tag")?;
        let Ok(tag) = tag.cast::<PyString>() else { return Ok(None) };
        let Ok(tag) = tag.to_str() else { return Ok(None) };
        let mut e = Element::new(tag);
        let attrib = obj.getattr("attrib")?;
        let Ok(attrib) = attrib.cast::<PyDict>() else { return Ok(None) };
        for (k, v) in attrib.iter() {
            let (Ok(k), Ok(v)) = (k.cast::<PyString>(), v.cast::<PyString>()) else { return Ok(None) };
            let (Ok(k), Ok(v)) = (k.to_str(), v.to_str()) else { return Ok(None) };
            e.attrs.push((k.to_string(), v.to_string()));
        }
        for (slot, name) in [(&mut e.text, "text"), (&mut e.tail, "tail")] {
            let v = obj.getattr(name)?;
            if v.is_none() {
                continue;
            }
            let Ok(s) = v.cast::<PyString>() else { return Ok(None) };
            let Ok(s) = s.to_str() else { return Ok(None) };
            *slot = Some(s.to_string());
        }
        Ok(Some(e))
    }
    let Some(root) = one(obj)? else { return Ok(None) };
    let mut stack: Vec<(Element, Bound<'_, pyo3::types::PyIterator>)> = vec![(root, obj.try_iter()?)];
    loop {
        let next = stack.last_mut().unwrap().1.next();
        match next {
            Some(child) => {
                let child = child?;
                let Some(e) = one(&child)? else { return Ok(None) };
                let it = child.try_iter()?;
                stack.push((e, it));
            }
            None => {
                let (done, _) = stack.pop().unwrap();
                match stack.last_mut() {
                    Some((parent, _)) => parent.children.push(done),
                    None => return Ok(Some(done)),
                }
            }
        }
    }
}

#[cfg(feature = "python")]
/// `ET.tostring(root, encoding='utf-8', xml_declaration=True)` for a Python element.
pub fn py_xml_bytes<'py>(root: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let py = root.py();
    if let Some(e) = element_from_py(root)? {
        if let Ok(raw) = crate::xml::tostring(&e) {
            return Ok(PyBytes::new(py, &raw));
        }
    }
    let kw = PyDict::new(py);
    kw.set_item("encoding", "utf-8")?;
    kw.set_item("xml_declaration", true)?;
    Ok(py.import("xml.etree.ElementTree")?.getattr("tostring")?.call((root,), Some(&kw))?.cast_into::<PyBytes>()?)
}

#[cfg(feature = "python")]
/// Exact-`int` check (`type(v) is int`) returning the value as i128 (saturating).
pub fn exact_int(v: &Bound<'_, PyAny>) -> Option<i128> {
    if !v.get_type().is(v.py().get_type::<pyo3::types::PyInt>()) {
        return None;
    }
    match v.extract::<i128>() {
        Ok(n) => Some(n),
        Err(_) => {
            let negative = v.lt(0).unwrap_or(false);
            Some(if negative { i128::MIN } else { i128::MAX })
        }
    }
}

#[cfg(feature = "python")]
/// Read a bytes object (or bytearray) as a slice-owning Vec only when needed.
pub fn bytes_of<'a>(v: &'a Bound<'_, PyAny>) -> PyResult<std::borrow::Cow<'a, [u8]>> {
    if let Ok(b) = v.cast::<PyBytes>() {
        return Ok(std::borrow::Cow::Borrowed(b.as_bytes()));
    }
    if let Ok(b) = v.cast::<pyo3::types::PyByteArray>() {
        return Ok(std::borrow::Cow::Owned(b.to_vec()));
    }
    Err(PyTypeError::new_err(format!("a bytes-like object is required, not '{}'", v.get_type().name()?)))
}

#[cfg(feature = "python")]
/// Build a Python `tools.notes_payload.PayloadResult` from a core
/// [`crate::payload::PayloadResult`] whose part ranges index `body` (a bytes
/// object). Uses the live Python-level classes, so it is safe to call from any
/// extension module (e.g. rs-ops after `rs_doc::payload::parse_payload`).
pub fn payload_result_to_python<'py>(
    body: &Bound<'py, PyAny>,
    content_type: &Bound<'py, PyAny>,
    result: &crate::payload::PayloadResult,
) -> PyResult<Bound<'py, PyAny>> {
    let py = body.py();
    let raw = body.cast::<PyBytes>()?;
    let data = raw.as_bytes();
    let m = py.import(PAYLOAD_MODULE)?;
    let opt = |v: &Option<String>| -> Bound<'py, PyAny> {
        match v {
            Some(s) => PyString::new(py, s).into_any(),
            None => py.None().into_bound(py),
        }
    };
    let part_cls = m.getattr("PayloadPart")?;
    let mut parts = Vec::with_capacity(result.parts.len());
    let mut bodies = Vec::with_capacity(result.parts.len());
    for p in &result.parts {
        let b = if p.body.start == 0 && p.body.end == data.len() { body.clone() } else { PyBytes::new(py, &data[p.body.clone()]).into_any() };
        parts.push(part_cls.call1((p.index, opt(&p.name), opt(&p.filename), PyString::new(py, &p.content_type), &b))?);
        bodies.push(b);
    }
    let docs: Vec<Bound<'py, PyAny>> = result.xml_parts.iter().map(|&i| bodies[i].clone()).collect();
    let url_cls = m.getattr("DownloadUrl")?;
    let urls: Vec<Bound<'py, PyAny>> = result.download_urls.iter().map(|u| url_cls.call1((&u.id, &u.url))).collect::<PyResult<_>>()?;
    let file_cls = m.getattr("ContentFile")?;
    let files: Vec<Bound<'py, PyAny>> = result
        .content_files
        .iter()
        .map(|f| file_cls.call1((&f.file_hash, opt(&f.source_id), opt(&f.metadata_hash), opt(&f.name), opt(&f.name_encoded), opt(&f.bind_number))))
        .collect::<PyResult<_>>()?;
    let preview_cls = m.getattr("TextPreview")?;
    let previews: Vec<Bound<'py, PyAny>> = result.text_previews.iter().map(|p| preview_cls.call1((p.context, &p.text))).collect::<PyResult<_>>()?;
    m.getattr("PayloadResult")?.call1((
        body,
        content_type,
        PyTuple::new(py, parts)?,
        PyTuple::new(py, docs)?,
        PyTuple::new(py, &result.xml_roots)?,
        PyTuple::new(py, &result.resource_ids)?,
        PyTuple::new(py, urls)?,
        PyTuple::new(py, files)?,
        PyTuple::new(py, previews)?,
        result.locked,
    ))
}
