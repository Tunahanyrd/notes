//! Python bindings replacing `samsung_notes.document`.
#[cfg(feature = "python")]
use pyo3::exceptions::{PyAttributeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyDict, PyFrozenSet, PyModule, PyString, PyTuple, PyType};

use crate::doc::{self, NowArg};
use crate::mime::Part;
use crate::notes_payload::value_class;
#[cfg(feature = "python")]
use crate::pyapi::{self, DocumentError, IntoPy};
use crate::util;
use crate::xml;

#[cfg(feature = "python")]
value_class!(PyDocumentSnapshot, "DocumentSnapshot", "samsung_notes.document",
    [title, body, editable, read_only_reason, local_id, commit_id, created_ms, modified_ms, resource_xml, raw_body, content_type], default server_id; {
    #[cfg(feature = "python")]
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("schemaVersion", 1)?;
        d.set_item("title", &self.title)?;
        d.set_item("body", &self.body)?;
        d.set_item("editable", &self.editable)?;
        d.set_item("read_only_reason", &self.read_only_reason)?;
        d.set_item("local_id", &self.local_id)?;
        d.set_item("commit_id", &self.commit_id)?;
        d.set_item("created_ms", &self.created_ms)?;
        d.set_item("modified_ms", &self.modified_ms)?;
        d.set_item("server_id", &self.server_id)?;
        d.set_item("content_type", &self.content_type)?;
        let resource = self.resource_xml.bind(py);
        let raw = self.raw_body.bind(py);
        let raw = pyapi::bytes_of(raw)?;
        d.set_item("resource_xml", util::b64encode(&pyapi::bytes_of(resource)?))?;
        d.set_item("raw_body", util::b64encode(&raw))?;
        d.set_item("raw_sha256", util::hex(&util::sha256(&raw)))?;
        Ok(d)
    }
    #[cfg(feature = "python")]
    #[classmethod]
    fn from_dict<'py>(_cls: &Bound<'py, PyType>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDocumentSnapshot>> {
        from_dict_impl(value)
    }
});

#[cfg(feature = "python")]
value_class!(PyDocumentEdit, "DocumentEdit", "samsung_notes.document",
    [full_xml, tree_xml, upload_xml, requested_ids, title, body]; {});

#[cfg(feature = "python")]
fn derr(msg: &str) -> PyErr {
    DocumentError::new_err(msg.to_string())
}

#[cfg(feature = "python")]
fn from_dict_impl<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDocumentSnapshot>> {
    let py = value.py();
    let limits = pyapi::doc_limits(py)?;
    let Ok(d) = value.cast::<PyDict>() else { return Err(derr("Kayıtlı belge sürümü desteklenmiyor.")) };
    let version = d.get_item("schemaVersion")?;
    let ok = match &version {
        Some(v) => v.eq(1)?,
        None => false,
    };
    if !ok {
        return Err(derr("Kayıtlı belge sürümü desteklenmiyor."));
    }
    let encoded = d.get_item("raw_body")?.unwrap_or_else(|| PyString::new(py, "").into_any());
    let Ok(encoded) = encoded.cast::<PyString>() else { return Err(derr("Kayıtlı belge boyutu geçersiz.")) };
    if encoded.len()? > (limits.max_body_bytes + 2) / 3 * 4 {
        return Err(derr("Kayıtlı belge boyutu geçersiz."));
    }
    let text = encoded.to_str().ok().filter(|s| s.is_ascii());
    let Some(text) = text else {
        return Err(PyValueError::new_err("string argument should contain only ASCII characters"));
    };
    let raw = match util::b64decode_strict(text.as_bytes()) {
        Some(r) => r,
        None => {
            let cls = py.import("binascii")?.getattr("Error")?;
            return Err(PyErr::from_value(cls.call1(("Invalid base64-encoded string",))?));
        }
    };
    let digest = util::hex(&util::sha256(&raw));
    let expected = d.get_item("raw_sha256")?;
    let same = match expected {
        Some(e) => e.eq(&digest)?,
        None => false,
    };
    if !same {
        return Err(derr("Kayıtlı belgenin bütünlük kontrolü başarısız."));
    }
    let content_type = value.get_item("content_type")?;
    let server_id = d.get_item("server_id")?.unwrap_or_else(|| py.None().into_bound(py));
    let raw_obj = PyBytes::new(py, &raw).into_any();
    parse_document_impl(&raw_obj, &content_type, &server_id)
}

#[cfg(feature = "python")]
fn opt_str(py: Python<'_>, v: &Option<String>) -> Py<PyAny> {
    match v {
        Some(s) => PyString::new(py, s).into_any().unbind(),
        None => py.None(),
    }
}

#[cfg(feature = "python")]
fn opt_int(py: Python<'_>, v: Option<i64>) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Some(n) => n.into_pyobject(py)?.into_any().unbind(),
        None => py.None(),
    })
}

#[cfg(feature = "python")]
pub(crate) fn parse_document_impl<'py>(
    raw: &Bound<'py, PyAny>,
    content_type: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDocumentSnapshot>> {
    let py = raw.py();
    let limits = pyapi::doc_limits(py)?;
    let Ok(bytes) = raw.cast::<PyBytes>() else { return Err(derr("Belge boş veya boyut sınırını aşıyor.")) };
    let data = bytes.as_bytes();
    if data.is_empty() || data.len() > limits.max_body_bytes {
        return Err(derr("Belge boş veya boyut sınırını aşıyor."));
    }
    let ct = match content_type.cast::<PyString>() {
        Ok(s) => s.to_str().map_err(|_| pyapi::PayloadError::new_err("invalid content type"))?,
        Err(_) => return Err(pyapi::PayloadError::new_err("invalid content type")),
    };
    let mut hooks = pyapi::PyHooks::new(py)?;
    let patched = pyapi::patched_any(py, "_parts", &pyapi::BUILTIN_PARTS)?;
    let max_readonly = limits.max_readonly_parts;
    let mut repart = || -> crate::Result<Option<(Vec<Part>, Vec<u8>)>> {
        let Some(f) = &patched else { return Ok(None) };
        let ext = |e: PyErr| crate::Error::External(Box::new(e));
        let kw = PyDict::new(py);
        kw.set_item("max_parts", max_readonly).map_err(ext)?;
        let returned = f.call((raw, content_type), Some(&kw)).map_err(ext)?;
        let (parts, buf, _) = crate::notes_payload::parts_from_python(&returned).map_err(ext)?;
        Ok(Some((parts, buf)))
    };
    let fields = doc::parse_document_fields(data, ct, &limits, &mut hooks, &mut repart).py(py);
    let f = fields?;
    Bound::new(py, PyDocumentSnapshot {
        title: PyString::new(py, &f.title).into_any().unbind(),
        body: PyString::new(py, &f.body).into_any().unbind(),
        editable: PyBool::new(py, f.editable).to_owned().into_any().unbind(),
        read_only_reason: opt_str(py, &f.read_only_reason),
        local_id: opt_str(py, &f.local_id),
        commit_id: opt_str(py, &f.commit_id),
        created_ms: opt_int(py, f.created_ms)?,
        modified_ms: opt_int(py, f.modified_ms)?,
        resource_xml: PyBytes::new(py, &f.resource_xml).into_any().unbind(),
        raw_body: raw.clone().unbind(),
        content_type: content_type.clone().unbind(),
        server_id: server_id.clone().unbind(),
    })
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (raw, content_type, *, server_id=None))]
fn parse_document<'py>(raw: &Bound<'py, PyAny>, content_type: &Bound<'py, PyAny>, server_id: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyDocumentSnapshot>> {
    let none = raw.py().None().into_bound(raw.py());
    parse_document_impl(raw, content_type, server_id.unwrap_or(&none))
}

#[cfg(feature = "python")]
/// `local(node)`: lower-cased local tag name.
#[pyfunction]
fn local(node: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    let py = node.py();
    let tag = node.getattr("tag")?;
    if let Ok(s) = tag.cast::<PyString>() {
        if let Ok(t) = s.to_str() {
            return Ok(PyString::new(py, &xml::local_lower(t)).into_any().unbind());
        }
    }
    Ok(tag.call_method1("rsplit", ("}", 1))?.get_item(-1)?.call_method0("lower")?.unbind())
}

#[cfg(feature = "python")]
fn local_eq(node: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    let tag = node.getattr("tag")?;
    if let Ok(s) = tag.cast::<PyString>() {
        if let Ok(t) = s.to_str() {
            return Ok(xml::local_lower(t) == name);
        }
    }
    let l = tag.call_method1("rsplit", ("}", 1))?.get_item(-1)?.call_method0("lower")?;
    l.eq(name)
}

#[cfg(feature = "python")]
#[pyfunction]
fn one<'py>(parent: &Bound<'py, PyAny>, name: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let name: Option<String> = name.extract().ok();
    let mut found: Option<Bound<'py, PyAny>> = None;
    let mut count = 0;
    for child in parent.try_iter()? {
        let child = child?;
        let hit = match &name {
            Some(n) => local_eq(&child, n)?,
            None => false,
        };
        if hit {
            count += 1;
            if found.is_none() {
                found = Some(child);
            }
        }
    }
    match (count, found) {
        (1, Some(c)) => Ok(c),
        _ => Err(derr("Belgenin yapısı güvenli düzenleme için uygun değil.")),
    }
}

#[cfg(feature = "python")]
fn text_str<'a>(value: &'a Bound<'_, PyAny>, max_text: usize) -> PyResult<&'a str> {
    let invalid = || derr("Metin geçersiz veya desteklenen boyutu aşıyor.");
    let s = value.cast::<PyString>().map_err(|_| invalid())?;
    let s = s.to_str().map_err(|_| invalid())?;
    doc::text_value(s, max_text).map_err(|_| invalid())?;
    Ok(s)
}

#[cfg(feature = "python")]
#[pyfunction]
fn text_value<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    text_str(value, doc::MAX_TEXT)?;
    Ok(value.clone())
}

#[cfg(feature = "python")]
#[pyfunction]
fn encode_text(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let s = text_str(value, doc::MAX_TEXT)?;
    Ok(crate::writer::double_b64(s))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (value, default=None))]
fn integer(py: Python<'_>, value: &Bound<'_, PyAny>, default: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
    if value.is_none() {
        if let Some(d) = default.filter(|d| !d.is_none(py)) {
            return Ok(d);
        }
    }
    let s = value.cast::<PyString>().ok().and_then(|s| s.to_str().ok());
    let n = doc::integer(s).py(py)?;
    Ok(n.into_pyobject(py)?.into_any().unbind())
}

#[cfg(feature = "python")]
#[pyfunction]
fn normalize_commit<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    let invalid = || derr("Senkronizasyon sürümü eksik veya geçersiz.");
    let s = value.cast::<PyString>().map_err(|_| invalid())?;
    match s.to_str() {
        Ok(t) => {
            let n = doc::normalize_commit(Some(t)).py(py)?;
            if n.len() == t.len() {
                Ok(value.clone())
            } else {
                Ok(PyString::new(py, n).into_any())
            }
        }
        Err(_) => {
            // lone surrogates: same rules through Python string operations
            let len = s.len()?;
            if len == 0 || len > 1024 {
                return Err(invalid());
            }
            for c in s.try_iter()? {
                let c: String = c?.extract().unwrap_or_default();
                if c.chars().next().is_some_and(|c| (c as u32) < 33) {
                    return Err(invalid());
                }
            }
            let index: isize = s.call_method1("find", ("_lc",))?.extract()?;
            if index > 0 {
                Ok(s.get_item(pyo3::types::PySlice::new(py, 0, index, 1))?)
            } else {
                Ok(value.clone())
            }
        }
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn xml_bytes<'py>(root: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    pyapi::py_xml_bytes(root)
}

#[cfg(feature = "python")]
fn now_arg(v: Option<&Bound<'_, PyAny>>) -> NowArg {
    match v.filter(|v| !v.is_none()) {
        None => NowArg::Missing,
        Some(v) => match pyapi::exact_int(v) {
            Some(n) => NowArg::Int(n),
            None => NowArg::Invalid,
        },
    }
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (title, body, *, local_id=None, now_ms=None))]
fn new_document<'py>(py: Python<'py>, title: &Bound<'py, PyAny>, body: &Bound<'py, PyAny>, local_id: Option<&Bound<'py, PyAny>>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    let limits = pyapi::doc_limits(py)?;
    let t = text_str(title, limits.max_text)?;
    let b = text_str(body, limits.max_text)?;
    let id: Option<String> = match local_id.filter(|v| !v.is_none()) {
        None => None,
        Some(v) => match v.cast::<PyString>() {
            Ok(s) => Some(s.to_str().map_err(|_| PyValueError::new_err("badly formed hexadecimal UUID string"))?.to_string()),
            Err(_) => return Err(PyAttributeError::new_err(format!("'{}' object has no attribute 'replace'", v.get_type().name()?))),
        },
    };
    let (identity, raw) = doc::new_document(t, b, id.as_deref(), now_arg(now_ms), limits.max_text).py(py)?;
    PyTuple::new(py, [PyString::new(py, &identity).into_any(), PyBytes::new(py, &raw).into_any()])
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, title, body, *, now_ms=None))]
fn prepare_edit<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>, body: &Bound<'py, PyAny>, now_ms: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyDocumentEdit>> {
    let limits = pyapi::doc_limits(py)?;
    if !snapshot.getattr("editable")?.is_truthy()? {
        let reason = snapshot.getattr("read_only_reason")?;
        if reason.is_truthy()? {
            return Err(PyErr::from_value(py.get_type::<DocumentError>().call1((reason,))?));
        }
        return Err(derr("Bu not salt okunur."));
    }
    let t = text_str(title, limits.max_text)?;
    let b = text_str(body, limits.max_text)?;
    let snap_title = snapshot.getattr("title")?;
    let snap_body = snapshot.getattr("body")?;
    let st = snap_title.cast::<PyString>().ok().and_then(|s| s.to_str().ok());
    let sb = snap_body.cast::<PyString>().ok().and_then(|s| s.to_str().ok());
    let resource = snapshot.getattr("resource_xml")?;
    let resource_bytes: Vec<u8> = match resource.cast::<PyString>() {
        Ok(s) => s.to_str()?.as_bytes().to_vec(),
        Err(_) => pyapi::bytes_of(&resource)?.into_owned(),
    };
    let src = doc::EditSource { editable: true, read_only_reason: None, title: st, body: sb, resource_xml: &resource_bytes };
    let edit = doc::prepare_edit(&src, t, b, now_arg(now_ms), &limits).py(py)?;
    let ids: Vec<Bound<'py, PyString>> = edit.requested_ids.iter().map(|s| PyString::new(py, s)).collect();
    Bound::new(py, PyDocumentEdit {
        full_xml: PyBytes::new(py, &edit.full_xml).into_any().unbind(),
        tree_xml: PyBytes::new(py, &edit.tree_xml).into_any().unbind(),
        upload_xml: PyBytes::new(py, &edit.upload_xml).into_any().unbind(),
        requested_ids: PyFrozenSet::new(py, ids)?.into_any().unbind(),
        title: title.clone().unbind(),
        body: body.clone().unbind(),
    })
}

#[cfg(feature = "python")]
#[pyfunction]
fn multipart<'py>(py: Python<'py>, xml: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let data = pyapi::bytes_of(xml)?;
    let (gz, ct) = doc::multipart(&data);
    PyTuple::new(py, [PyBytes::new(py, &gz).into_any(), PyString::new(py, &ct).into_any()])
}

#[cfg(feature = "python")]
#[pyfunction]
fn page_inventory(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<String> {
    let resource = snapshot.getattr("resource_xml")?;
    let data: Vec<u8> = match resource.cast::<PyString>() {
        Ok(s) => s.to_str()?.as_bytes().to_vec(),
        Err(_) => pyapi::bytes_of(&resource)?.into_owned(),
    };
    doc::page_inventory(&data).py(py)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("DocumentError", py.get_type::<DocumentError>())?;
    py.get_type::<DocumentError>().setattr("__module__", "samsung_notes.document")?;
    m.add_class::<PyDocumentSnapshot>()?;
    m.add_class::<PyDocumentEdit>()?;
    m.add_function(wrap_pyfunction!(local, m)?)?;
    m.add_function(wrap_pyfunction!(one, m)?)?;
    m.add_function(wrap_pyfunction!(text_value, m)?)?;
    m.add_function(wrap_pyfunction!(encode_text, m)?)?;
    m.add_function(wrap_pyfunction!(integer, m)?)?;
    m.add_function(wrap_pyfunction!(normalize_commit, m)?)?;
    m.add_function(wrap_pyfunction!(xml_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(parse_document, m)?)?;
    m.add_function(wrap_pyfunction!(new_document, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_edit, m)?)?;
    m.add_function(wrap_pyfunction!(multipart, m)?)?;
    m.add_function(wrap_pyfunction!(page_inventory, m)?)?;
    rs_common::export(m, &[
        "DocumentError", "DocumentSnapshot", "DocumentEdit", "local", "one", "text_value", "encode_text",
        "integer", "normalize_commit", "xml_bytes", "parse_document", "new_document", "prepare_edit",
        "multipart", "page_inventory",
    ])
}
