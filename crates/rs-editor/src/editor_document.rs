//! Replacement for the Python module `editor_document`: thin PyO3 wrappers
//! over the pure-Rust core in `rich`, `edit`, `etree` and `pyval`.
#[cfg(feature = "python")]
use pyo3::exceptions::{PyAttributeError, PyOverflowError, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt as PyIntType, PyList, PyString, PyTuple};
#[cfg(feature = "python")]
use pyo3::IntoPyObjectExt;


use crate::edit::{self, DocBackend, Now, Snapshot};
use crate::pyval::{PyInt, Val};
use crate::rich::{self, Error, ExtKind, Record, SecIn};

const DOCUMENT: &str = "samsung_notes.document";

// ---- errors -------------------------------------------------------------

#[cfg(feature = "python")]
fn ext(py: Python<'_>, err: PyErr) -> Error {
    let is = |module: &str, name: &str| -> bool {
        py.import(module).and_then(|m| m.getattr(name)).map(|cls| err.matches(py, cls).unwrap_or(false)).unwrap_or(false)
    };
    let kind = if is(DOCUMENT, "DocumentError") { ExtKind::Document }
        else if is("xml.etree.ElementTree", "ParseError") { ExtKind::Parse }
        else if err.is_instance_of::<PyValueError>(py) { ExtKind::Value }
        else if err.is_instance_of::<PyTypeError>(py) { ExtKind::Type }
        else { ExtKind::Other };
    let message = err.value(py).str().map(|s| s.to_string()).unwrap_or_default();
    Error::External { kind, message, payload: Box::new(err) }
}

#[cfg(feature = "python")]
fn raise(py: Python<'_>, module: &str, name: &str, message: String) -> PyErr {
    match py.import(module).and_then(|m| m.getattr(name)).and_then(|cls| cls.call1((message,))) {
        Ok(value) => PyErr::from_value(value),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
fn to_pyerr(py: Python<'_>, e: Error) -> PyErr {
    match e {
        Error::Document(m) => rs_doc::pyapi::document_error(py, &m),
        Error::Value(m) => PyValueError::new_err(m),
        Error::Type(m) => PyTypeError::new_err(m),
        Error::Overflow(m) => PyOverflowError::new_err(m),
        Error::Attribute(m) => PyAttributeError::new_err(m),
        Error::Parse(m) => raise(py, "xml.etree.ElementTree", "ParseError", m),
        Error::Payload(m) => rs_doc::pyapi::payload_error(py, &m),
        Error::External { payload, message, .. } => match payload.downcast::<PyErr>() {
            Ok(err) => *err,
            Err(_) => PyValueError::new_err(message),
        },
    }
}

trait OrRaise<T> {
    #[cfg(feature = "python")]
    fn or_raise(self, py: Python<'_>) -> PyResult<T>;
}

impl<T> OrRaise<T> for rich::Result<T> {
    #[cfg(feature = "python")]
    fn or_raise(self, py: Python<'_>) -> PyResult<T> {
        self.map_err(|e| to_pyerr(py, e))
    }
}

// ---- values -------------------------------------------------------------

#[cfg(feature = "python")]
fn int_repr(obj: &Bound<'_, PyAny>) -> PyResult<String> {
    match obj.extract::<i64>() {
        Ok(v) => Ok(v.to_string()),
        Err(_) => Ok(obj.py().import("builtins")?.getattr("int")?.getattr("__repr__")?.call1((obj,))?.str()?.to_string()),
    }
}

#[cfg(feature = "python")]
fn to_val(obj: &Bound<'_, PyAny>, depth: usize) -> PyResult<Val> {
    if obj.is_none() { return Ok(Val::None); }
    if obj.is_instance_of::<PyBool>() { return Ok(Val::Bool(obj.is_truthy()?)); }
    if obj.is_instance_of::<PyIntType>() {
        return Ok(Val::Int(PyInt(int_repr(obj)?), obj.is_exact_instance_of::<PyIntType>()));
    }
    if obj.is_instance_of::<PyFloat>() { return Ok(Val::Float(obj.extract::<f64>()?)); }
    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(match s.to_str() { Ok(t) => Val::Str(t.to_string()), Err(_) => Val::BadStr(obj.len()?) });
    }
    if depth < 8 {
        if let Ok(list) = obj.cast::<PyList>() {
            return Ok(Val::List(list.iter().map(|x| to_val(&x, depth + 1)).collect::<PyResult<_>>()?));
        }
        if let Ok(dict) = obj.cast::<PyDict>() {
            let mut items = Vec::new();
            for (k, v) in dict.iter() { items.push((to_val(&k, depth + 1)?, to_val(&v, depth + 1)?)); }
            return Ok(Val::Dict(items));
        }
    }
    let float = obj.py().import("builtins")?.getattr("float")?.call1((obj,)).ok().and_then(|f| f.extract::<f64>().ok());
    Ok(Val::Other(float))
}

#[cfg(feature = "python")]
fn from_val<'py>(py: Python<'py>, v: &Val) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        Val::None | Val::BadStr(_) | Val::Other(_) => py.None().into_bound(py),
        Val::Bool(b) => b.into_bound_py_any(py)?,
        Val::Int(i, _) => match i.to_i64() {
            Some(x) => x.into_bound_py_any(py)?,
            None => py.import("builtins")?.getattr("int")?.call1((i.0.as_str(),))?,
        },
        Val::Float(f) => f.into_bound_py_any(py)?,
        Val::Str(s) => PyString::new(py, s).into_any(),
        Val::List(items) => PyList::new(py, items.iter().map(|x| from_val(py, x)).collect::<PyResult<Vec<_>>>()?)?.into_any(),
        Val::Dict(items) => {
            let d = PyDict::new(py);
            for (k, x) in items { d.set_item(from_val(py, k)?, from_val(py, x)?)?; }
            d.into_any()
        }
    })
}

#[cfg(feature = "python")]
fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type().name().map(|n| n.to_string()).unwrap_or_else(|_| "object".into())
}

#[cfg(feature = "python")]
fn records(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Record>> {
    let mut out = Vec::new();
    for item in obj.try_iter()? {
        let item = item?;
        out.push(match item.cast::<PyString>() {
            Ok(s) => Ok(s.to_string_lossy().into_owned()),
            Err(_) => Err(type_name(&item)),
        });
    }
    Ok(out)
}

#[cfg(feature = "python")]
fn sections_tuple<'py>(py: Python<'py>, sections: &[rich::SectionT]) -> PyResult<Bound<'py, PyTuple>> {
    let items: Vec<Bound<'py, PyTuple>> = sections.iter()
        .map(|(a, b, r)| PyTuple::new(py, [a.into_bound_py_any(py)?, b.into_bound_py_any(py)?, PyString::new(py, r).into_any()]))
        .collect::<PyResult<_>>()?;
    PyTuple::new(py, items)
}

// ---- RichText -------------------------------------------------------------

#[cfg(feature = "python")]
/// Frozen dataclass-like rich text value (fields as in the Python dataclass).
#[pyclass(frozen, subclass, module = "samsung_notes.editor_document", name = "RichText")]
pub struct RichText {
    #[pyo3(get)]
    encoded: Py<PyAny>,
    #[pyo3(get)]
    model: Py<PyAny>,
    #[pyo3(get)]
    span_order: Py<PyAny>,
    #[pyo3(get)]
    zero_length: Py<PyAny>,
    #[pyo3(get)]
    sections: Py<PyAny>,
}

#[cfg(feature = "python")]
impl RichText {
    #[cfg(feature = "python")]
    fn fields<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, [&self.encoded, &self.model, &self.span_order, &self.zero_length, &self.sections].map(|f| f.bind(py).clone()))
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl RichText {
    #[cfg(feature = "python")]
    #[new]
    #[pyo3(signature = (encoded, model, span_order, zero_length, sections=None))]
    fn new(py: Python<'_>, encoded: Py<PyAny>, model: Py<PyAny>, span_order: Py<PyAny>, zero_length: Py<PyAny>,
           sections: Option<Py<PyAny>>) -> Self {
        let sections = sections.unwrap_or_else(|| PyTuple::empty(py).into_any().unbind());
        RichText { encoded, model, span_order, zero_length, sections }
    }

    #[cfg(feature = "python")]
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        py.import("copy")?.getattr("deepcopy")?.call1((self.model.bind(py),))
    }

    #[cfg(feature = "python")]
    fn __eq__<'py>(slf: &Bound<'py, Self>, other: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        if !other.get_type().is(slf.get_type()) {
            return Ok(py.NotImplemented().into_bound(py));
        }
        let other = other.cast::<RichText>()?;
        slf.get().fields(py)?.eq(other.get().fields(py)?)?.into_bound_py_any(py)
    }

    #[cfg(feature = "python")]
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        self.fields(py)?.hash()
    }

    #[cfg(feature = "python")]
    #[classattr]
    fn __match_args__(py: Python<'_>) -> PyResult<Py<PyTuple>> {
        Ok(PyTuple::new(py, ["encoded", "model", "span_order", "zero_length", "sections"])?.unbind())
    }
}

#[cfg(feature = "python")]
fn rich_object<'py>(py: Python<'py>, encoded: Bound<'py, PyAny>, model: &Val, span_order: Bound<'py, PyAny>,
                    zero_length: Bound<'py, PyAny>, sections: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let value = RichText { encoded: encoded.unbind(), model: from_val(py, model)?.unbind(), span_order: span_order.unbind(),
        zero_length: zero_length.unbind(), sections: sections.unbind() };
    Ok(Bound::new(py, value)?.into_any())
}

// ---- snapshot / backend ---------------------------------------------------

#[cfg(feature = "python")]
fn resolve<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> rich::Result<(Bound<'py, PyAny>, Snapshot)> {
    let cls = document_attr(py, "DocumentSnapshot").map_err(|e| ext(py, e))?;
    let snap = if snapshot.is_instance_of::<PyDict>() {
        cls.call_method1("from_dict", (snapshot,)).map_err(|e| ext(py, e))?
    } else { snapshot.clone() };
    let valid = snap.is_instance(&cls).map_err(|e| ext(py, e))?
        && snap.getattr("commit_id").and_then(|c| c.is_truthy()).map_err(|e| ext(py, e))?;
    if !valid { return rich::doc("Notun Cloud sürümü eksik."); }
    let title = snap.getattr("title").map_err(|e| ext(py, e))?.extract::<String>().ok();
    let xml = snap.getattr("resource_xml").map_err(|e| ext(py, e))?;
    let resource_xml = if let Ok(b) = xml.cast::<PyBytes>() { b.as_bytes().to_vec() }
        else if let Ok(s) = xml.cast::<PyString>() { s.to_string_lossy().into_owned().into_bytes() }
        else { return Err(Error::Type("a bytes-like object is required".into())); };
    Ok((snap, Snapshot { title, resource_xml }))
}

#[cfg(feature = "python")]
/// Python-visible objects of samsung_notes.document (Rust-backed under
/// NOTES_IMPL=rust; never constructed from this extension's own types).
fn document_attr<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import(DOCUMENT)?.getattr(name)
}

#[cfg(feature = "python")]
fn backend(py: Python<'_>, snapshot: Option<&Bound<'_, PyAny>>) -> PyResult<DocBackend> {
    let limits = rs_doc::pyapi::doc_limits(py)?;
    let (raw_body, content_type) = match snapshot {
        Some(snap) => (snap.getattr("raw_body")?.extract::<Vec<u8>>().unwrap_or_default(),
                       snap.getattr("content_type")?.extract::<String>().unwrap_or_default()),
        None => (Vec::new(), String::new()),
    };
    Ok(DocBackend { limits, raw_body, content_type })
}

#[cfg(feature = "python")]
fn now_arg(now_ms: Option<&Bound<'_, PyAny>>) -> PyResult<Now> {
    match now_ms {
        Some(n) if !n.is_none() => Ok(if n.is_exact_instance_of::<PyIntType>() { Now::Int(PyInt(int_repr(n)?)) } else { Now::NotInt }),
        _ => {
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            Ok(Now::Int(PyInt::from_i128(ms as i128)))
        }
    }
}

#[cfg(feature = "python")]
fn build_edit<'py>(py: Python<'py>, out: edit::EditOut, snap: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let ids: Vec<Bound<'py, PyAny>> = out.requested_ids.iter()
        .map(|id| match id { Some(s) => PyString::new(py, s).into_any(), None => py.None().into_bound(py) }).collect();
    let body = match out.body { Some(b) => PyString::new(py, &b).into_any(), None => snap.getattr("body")? };
    document_attr(py, "DocumentEdit")?.call1((PyBytes::new(py, &out.full_xml), PyBytes::new(py, &out.tree_xml),
        PyBytes::new(py, &out.upload_xml), PyFrozenSet::new(py, ids)?, PyString::new(py, &out.title), body))
}

// ---- functions ----------------------------------------------------------

#[cfg(feature = "python")]
#[pyfunction]
fn utf16_length(text: &Bound<'_, PyAny>) -> PyResult<usize> {
    match text.cast::<PyString>().ok().and_then(|s| s.to_str().ok()) {
        Some(s) => Ok(rich::utf16_length(s)),
        None => Ok(text.call_method1("encode", ("utf-16-le",))?.len()? / 2),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn normalize_model<'py>(py: Python<'py>, model: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let value = to_val(model, 0)?;
    from_val(py, &rich::normalize_model(&value).or_raise(py)?)
}

#[cfg(feature = "python")]
#[pyfunction]
fn editor_model_fingerprint(py: Python<'_>, model: &Bound<'_, PyAny>) -> PyResult<String> {
    rich::editor_model_fingerprint(&to_val(model, 0)?).or_raise(py)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (encoded, *, span_order=None, zero_length=None))]
fn read_rich_text<'py>(py: Python<'py>, encoded: Bound<'py, PyAny>, span_order: Option<Bound<'py, PyAny>>,
                       zero_length: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let span_order = span_order.unwrap_or_else(|| PyTuple::empty(py).into_any());
    let zero_length = zero_length.unwrap_or_else(|| PyTuple::empty(py).into_any());
    if span_order.len()? + zero_length.len()? > rich::MAX_ANNOTATIONS * 2 {
        return Err(to_pyerr(py, Error::Document("Biçimlendirme sırası sınırı aşıldı.".into())));
    }
    let text = match encoded.cast::<PyString>() {
        Ok(s) => match s.to_str() {
            Ok(t) => t.to_string(),
            Err(_) => {
                let message = if encoded.len()? > rich::MAX_ENCODED { "Biçimlendirme verisi boyut sınırını aşıyor." } else { "Biçimlendirme kodlaması geçersiz." };
                return Err(to_pyerr(py, Error::Document(message.into())));
            }
        },
        Err(_) => return Err(to_pyerr(py, Error::Document("Biçimlendirme verisi boyut sınırını aşıyor.".into()))),
    };
    let rich = rich::read_rich_text(&text, &records(&span_order)?, &records(&zero_length)?).or_raise(py)?;
    let so = PyTuple::new(py, span_order.try_iter()?.collect::<PyResult<Vec<_>>>()?)?.into_any();
    let zl = PyTuple::new(py, zero_length.try_iter()?.collect::<PyResult<Vec<_>>>()?)?.into_any();
    rich_object(py, encoded, &rich.model, so, zl, sections_tuple(py, &rich.sections)?.into_any())
}

#[cfg(feature = "python")]
fn unpack(item: &Bound<'_, PyAny>, tuple_input: bool) -> PyResult<SecIn> {
    let known = tuple_input || item.is_instance_of::<PyTuple>() || item.is_instance_of::<PyList>();
    let values: Vec<Bound<'_, PyAny>> = if known {
        item.try_iter()?.collect::<PyResult<_>>()?
    } else {
        let mut v = Vec::new();
        for x in item.try_iter()? {
            v.push(x?);
            if v.len() > 3 { return Ok(SecIn::Unpack(rich::unpack_message(4, false))); }
        }
        v
    };
    if values.len() != 3 { return Ok(SecIn::Unpack(rich::unpack_message(values.len(), true))); }
    Ok(SecIn::Ok(to_val(&values[0], 0)?, to_val(&values[1], 0)?, to_val(&values[2], 0)?))
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (model, sections=None))]
fn encode_rich_text<'py>(py: Python<'py>, model: &Bound<'py, PyAny>, sections: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let normalized = rich::normalize_model(&to_val(model, 0)?).or_raise(py)?;
    let mut tuples = Vec::new();
    if let Some(sections) = sections {
        for item in sections.try_iter()? {
            tuples.push(PyTuple::new(py, item?.try_iter()?.collect::<PyResult<Vec<_>>>()?)?);
        }
    }
    let items: Vec<SecIn> = tuples.iter().map(|t| unpack(t.as_any(), true)).collect::<PyResult<_>>()?;
    let checked = rich::check_sections(rich::model_text(&normalized), &items).or_raise(py)?;
    let rich = rich::encode_normalized(normalized, checked).or_raise(py)?;
    let strings = |v: &[String]| -> PyResult<Bound<'py, PyAny>> { Ok(PyTuple::new(py, v.iter().map(|s| PyString::new(py, s)))?.into_any()) };
    rich_object(py, PyString::new(py, &rich.encoded).into_any(), &rich.model, strings(&rich.span_order)?,
        strings(&rich.zero_length)?, PyTuple::new(py, tuples)?.into_any())
}

#[cfg(feature = "python")]
#[pyfunction]
fn remap_sections<'py>(py: Python<'py>, old_text: &Bound<'py, PyAny>, new_text: &Bound<'py, PyAny>,
                       sections: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    if !sections.is_truthy()? || old_text.eq(new_text)? {
        return PyTuple::new(py, sections.try_iter()?.collect::<PyResult<Vec<_>>>()?);
    }
    let old: String = old_text.extract()?;
    let new: String = new_text.extract()?;
    let mut positions = Vec::new();
    let mut raws = Vec::new();
    for item in sections.try_iter()? {
        let item = item?;
        match unpack(&item, false)? {
            SecIn::Unpack(message) => return Err(PyValueError::new_err(message)),
            SecIn::Ok(a, b, _) => {
                let (Some(a), Some(b)) = (a.exact_i64(), b.exact_i64()) else {
                    return Err(to_pyerr(py, Error::Document("Sayfa bölümleri metinle uyuşmuyor.".into())));
                };
                let values: Vec<Bound<'py, PyAny>> = item.try_iter()?.collect::<PyResult<_>>()?;
                positions.push((a, b));
                raws.push(values[2].clone());
            }
        }
    }
    let moved = rich::remap_offsets(&old, &new, &positions);
    let mut out = Vec::new();
    let mut check = Vec::new();
    for (((start, end), (ns, ne)), raw) in positions.iter().zip(moved).zip(raws) {
        let raw = if ne - ns == end - start { raw } else { PyString::new(py, &(ne - ns).to_string()).into_any() };
        check.push(SecIn::Ok(Val::int(ns as i128), Val::int(ne as i128), to_val(&raw, 0)?));
        out.push(PyTuple::new(py, [ns.into_bound_py_any(py)?, ne.into_bound_py_any(py)?, raw])?);
    }
    rich::check_sections(&new, &check).or_raise(py)?;
    PyTuple::new(py, out)
}

#[cfg(feature = "python")]
fn fingerprint_input(py: Python<'_>, resource_xml: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    match resource_xml.cast::<PyBytes>() {
        Ok(b) => Ok(b.as_bytes().to_vec()),
        Err(_) => Err(to_pyerr(py, Error::Document("Belge parmak izi girdisi geçersiz.".into()))),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn title_content_fingerprint(py: Python<'_>, resource_xml: &Bound<'_, PyAny>) -> PyResult<String> {
    edit::title_content_fingerprint(&fingerprint_input(py, resource_xml)?).or_raise(py)
}

#[cfg(feature = "python")]
#[pyfunction]
fn editor_content_fingerprint(py: Python<'_>, resource_xml: &Bound<'_, PyAny>) -> PyResult<String> {
    edit::editor_content_fingerprint(&fingerprint_input(py, resource_xml)?).or_raise(py)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, title, *, now_ms=None))]
fn prepare_title_edit<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>,
                           now_ms: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let (snap_obj, snap) = resolve(py, snapshot).or_raise(py)?;
    let title = to_val(title, 0)?;
    let now = now_arg(now_ms.as_ref())?;
    let backend = backend(py, Some(&snap_obj))?;
    let out = edit::prepare_title_edit(&snap, &title, &now, &backend).or_raise(py)?;
    build_edit(py, out, &snap_obj)
}

#[cfg(feature = "python")]
#[pyfunction]
fn editor_projection<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let (snap_obj, snap) = match resolve(py, snapshot) {
        Ok((o, s)) => (Some(o), Ok(s)),
        Err(e) => (None, Err(e)),
    };
    let backend = backend(py, snap_obj.as_ref())?;
    let p = match &snap {
        Ok(s) => edit::editor_projection(Ok(s), &backend),
        Err(_) => edit::editor_projection(Err(snap.err().unwrap()), &backend),
    }.or_raise(py)?;
    let caps = PyDict::new(py);
    caps.set_item("titleEdit", p.title_edit)?;
    caps.set_item("textEdit", p.text_edit)?;
    caps.set_item("richStyle", p.text_edit)?;
    caps.set_item("inkEdit", false)?;
    let result = PyDict::new(py);
    result.set_item("schemaVersion", 1)?;
    result.set_item("capabilities", caps)?;
    result.set_item("model", match &p.model { Some(m) => from_val(py, m)?, None => py.None().into_bound(py) })?;
    result.set_item("reason", p.reason)?;
    Ok(result.into_any())
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (title, model, *, local_id=None, now_ms=None))]
fn new_editor_document<'py>(py: Python<'py>, title: Bound<'py, PyAny>, model: &Bound<'py, PyAny>,
                            local_id: Option<Bound<'py, PyAny>>, now_ms: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyTuple>> {
    let value = to_val(model, 0)?;
    let (identity, xml) = edit::new_editor_document(&value, |text| {
        let plain_title = title.cast::<PyString>().ok().and_then(|t| t.to_str().ok().map(str::to_string));
        let plain_id = match &local_id {
            None => Some(None),
            Some(v) if v.is_none() => Some(None),
            Some(v) => v.cast::<PyString>().ok().and_then(|t| t.to_str().ok()).map(|t| Some(t.to_string())),
        };
        let now = match &now_ms {
            None => Some(rs_doc::doc::NowArg::Missing),
            Some(v) if v.is_none() => Some(rs_doc::doc::NowArg::Missing),
            Some(v) if v.is_exact_instance_of::<PyIntType>() => v.extract::<i128>().ok().map(rs_doc::doc::NowArg::Int),
            Some(_) => Some(rs_doc::doc::NowArg::Invalid),
        };
        if let (Some(t), Some(id), Some(now)) = (plain_title, plain_id, now) {
            let max_text = rs_doc::pyapi::doc_limits(py).map_err(|e| ext(py, e))?.max_text;
            let (identity, xml) = rs_doc::doc::new_document(&t, text, id.as_deref(), now, max_text)?;
            return Ok((PyString::new(py, &identity).into_any(), xml));
        }
        // Unusual argument types: same function through samsung_notes.document.
        let run = || -> PyResult<(Bound<'py, PyAny>, Vec<u8>)> {
            let kwargs = PyDict::new(py);
            kwargs.set_item("local_id", local_id.as_ref())?;
            kwargs.set_item("now_ms", now_ms.as_ref())?;
            let out = document_attr(py, "new_document")?.call((&title, text), Some(&kwargs))?;
            let identity = out.get_item(0)?;
            let xml = out.get_item(1)?.extract::<Vec<u8>>()?;
            Ok((identity, xml))
        };
        run().map_err(|e| ext(py, e))
    }).or_raise(py)?;
    PyTuple::new(py, [identity, PyBytes::new(py, &xml).into_any()])
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, title, model, *, now_ms=None))]
fn prepare_editor_edit<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>,
                            model: &Bound<'py, PyAny>, now_ms: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let (snap_obj, snap) = resolve(py, snapshot).or_raise(py)?;
    let title = to_val(title, 0)?;
    let model = to_val(model, 0)?;
    let now = now_arg(now_ms.as_ref())?;
    let backend = backend(py, Some(&snap_obj))?;
    let out = edit::prepare_editor_edit(&snap, &title, &model, &now, &backend).or_raise(py)?;
    build_edit(py, out, &snap_obj)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RichText>()?;
    m.add_function(wrap_pyfunction!(utf16_length, m)?)?;
    m.add_function(wrap_pyfunction!(normalize_model, m)?)?;
    m.add_function(wrap_pyfunction!(editor_model_fingerprint, m)?)?;
    m.add_function(wrap_pyfunction!(read_rich_text, m)?)?;
    m.add_function(wrap_pyfunction!(encode_rich_text, m)?)?;
    m.add_function(wrap_pyfunction!(remap_sections, m)?)?;
    m.add_function(wrap_pyfunction!(title_content_fingerprint, m)?)?;
    m.add_function(wrap_pyfunction!(editor_content_fingerprint, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_title_edit, m)?)?;
    m.add_function(wrap_pyfunction!(editor_projection, m)?)?;
    m.add_function(wrap_pyfunction!(new_editor_document, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_editor_edit, m)?)?;
    m.add("prepare_rich_edit", m.getattr("prepare_editor_edit")?)?;
    rs_common::export(m, &["RichText", "utf16_length", "normalize_model", "editor_model_fingerprint", "read_rich_text",
        "encode_rich_text", "remap_sections", "title_content_fingerprint", "editor_content_fingerprint",
        "prepare_title_edit", "editor_projection", "new_editor_document", "prepare_editor_edit", "prepare_rich_edit"])
}
