//! PyO3 boundary: exceptions, ElementTree conversion and the `Deps`
//! implementation backed by the Python doc/ink modules (PYFALLBACK until the
//! rs-doc / rs-ink crates are marked STABLE).
use crate::deps::*;
use crate::err::{Error, Kind, R};
use crate::xml::Element;
#[cfg(feature = "python")]
use pyo3::exceptions::*;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::sync::PyOnceLock;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple, PyType};
use std::any::Any;

#[cfg(feature = "python")]
static PAGE_OPS_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();
#[cfg(feature = "python")]
static SHAPE_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();
#[cfg(feature = "python")]
/// The Rust `verified_resource` builtin, to detect a patched module attribute.
pub static VERIFIED_FN: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

#[cfg(feature = "python")]
pub fn page_ops_error(py: Python<'_>) -> PyResult<Bound<'_, PyType>> {
    PAGE_OPS_ERROR
        .get_or_try_init(py, || {
            let base = py.import("samsung_notes.document")?.getattr("DocumentError")?.cast_into::<PyType>()?;
            PyErr::new_type(py, c"samsung_notes.page_ops.PageOpsError", None, Some(&base), None)
        })
        .map(|t| t.bind(py).clone())
}

#[cfg(feature = "python")]
pub fn shape_error(py: Python<'_>) -> PyResult<Bound<'_, PyType>> {
    SHAPE_ERROR
        .get_or_try_init(py, || {
            let base = py.import("samsung_notes.document")?.getattr("DocumentError")?.cast_into::<PyType>()?;
            PyErr::new_type(py, c"samsung_notes.page_shapes.ShapeError", Some(c"Shape cannot be written safely (Turkish user-facing message)."), Some(&base), None)
        })
        .map(|t| t.bind(py).clone())
}

#[cfg(feature = "python")]
fn class_for(py: Python<'_>, kind: Kind) -> PyResult<Bound<'_, PyAny>> {
    Ok(match kind {
        Kind::Ink => py.import("samsung_notes.ink")?.getattr("InkError")?,
        Kind::Doc => py.import("samsung_notes.document")?.getattr("DocumentError")?,
        Kind::PageOps => page_ops_error(py)?.into_any(),
        Kind::Shape => shape_error(py)?.into_any(),
        Kind::Payload => py.import("tools.notes_payload")?.getattr("PayloadError")?,
        Kind::Parse => py.import("xml.etree.ElementTree")?.getattr("ParseError")?,
        Kind::Value => py.get_type::<PyValueError>().into_any(),
        Kind::Type => py.get_type::<PyTypeError>().into_any(),
        Kind::Overflow => py.get_type::<PyOverflowError>().into_any(),
        Kind::Key => py.get_type::<PyKeyError>().into_any(),
        Kind::Unicode => py.get_type::<PyUnicodeError>().into_any(),
        Kind::Stop => py.get_type::<PyStopIteration>().into_any(),
        Kind::Attribute => py.get_type::<PyAttributeError>().into_any(),
        Kind::ZeroDiv => py.get_type::<PyZeroDivisionError>().into_any(),
        Kind::Other => py.get_type::<PyRuntimeError>().into_any(),
    })
}

#[cfg(feature = "python")]
/// Rust error → Python exception (foreign Python errors re-raised unchanged).
pub fn to_py(py: Python<'_>, e: Error) -> PyErr {
    if let Some(ext) = e.ext {
        if let Ok(err) = ext.downcast::<PyErr>() {
            return *err;
        }
    }
    let cls = match class_for(py, e.kind) {
        Ok(c) => c,
        Err(x) => return x,
    };
    let made = if e.kind == Kind::Stop { cls.call0() } else { cls.call1((e.msg,)) };
    match made {
        Ok(v) => PyErr::from_value(v),
        Err(x) => x,
    }
}

#[cfg(feature = "python")]
/// Python exception → Rust error classified by `isinstance`.
pub fn from_py(py: Python<'_>, err: PyErr) -> Error {
    let value = err.value(py);
    let test = |kind: Kind| -> bool { class_for(py, kind).ok().is_some_and(|c| value.is_instance(&c).unwrap_or(false)) };
    let order = [
        Kind::Shape,
        Kind::PageOps,
        Kind::Ink,
        Kind::Doc,
        Kind::Payload,
        Kind::Parse,
        Kind::Unicode,
        Kind::Value,
        Kind::Type,
        Kind::Overflow,
        Kind::Key,
        Kind::Stop,
        Kind::Attribute,
        Kind::ZeroDiv,
    ];
    let kind = order.into_iter().find(|k| test(*k)).unwrap_or(Kind::Other);
    let msg = value.str().map(|s| s.to_string()).unwrap_or_default();
    Error { kind, msg, ext: Some(Box::new(err)) }
}

pub trait PyR<T> {
    #[cfg(feature = "python")]
    fn r(self, py: Python<'_>) -> R<T>;
}

#[cfg(feature = "python")]
impl<T> PyR<T> for PyResult<T> {
    #[cfg(feature = "python")]
    fn r(self, py: Python<'_>) -> R<T> {
        self.map_err(|e| from_py(py, e))
    }
}

pub trait RPy<T> {
    #[cfg(feature = "python")]
    fn py(self, py: Python<'_>) -> PyResult<T>;
}

impl<T> RPy<T> for R<T> {
    #[cfg(feature = "python")]
    fn py(self, py: Python<'_>) -> PyResult<T> {
        self.map_err(|e| to_py(py, e))
    }
}

// ---------------------------------------------------------------- ElementTree

#[cfg(feature = "python")]
pub fn to_et<'py>(py: Python<'py>, e: &Element) -> PyResult<Bound<'py, PyAny>> {
    let cls = py.import("xml.etree.ElementTree")?.getattr("Element")?;
    build_et(py, &cls, e)
}

#[cfg(feature = "python")]
fn build_et<'py>(py: Python<'py>, cls: &Bound<'py, PyAny>, e: &Element) -> PyResult<Bound<'py, PyAny>> {
    let attrib = PyDict::new(py);
    for (k, v) in &e.attrib {
        attrib.set_item(k, v)?;
    }
    let node = cls.call1((e.tag.as_str(), attrib))?;
    if let Some(t) = &e.text {
        node.setattr("text", t)?;
    }
    if let Some(t) = &e.tail {
        node.setattr("tail", t)?;
    }
    for c in &e.children {
        node.call_method1("append", (build_et(py, cls, c)?,))?;
    }
    Ok(node)
}

#[cfg(feature = "python")]
pub fn from_et(node: &Bound<'_, PyAny>) -> PyResult<Element> {
    let mut e = Element { tag: node.getattr("tag")?.extract()?, ..Default::default() };
    let attrib = node.getattr("attrib")?;
    for (k, v) in attrib.cast::<PyDict>()?.iter() {
        e.attrib.push((k.extract()?, v.extract()?));
    }
    e.text = node.getattr("text")?.extract()?;
    e.tail = node.getattr("tail")?.extract()?;
    for c in node.try_iter()? {
        e.children.push(from_et(&c?)?);
    }
    Ok(e)
}

// ---------------------------------------------------------------- extraction

#[cfg(feature = "python")]
pub fn samples_from(obj: &Bound<'_, PyAny>) -> PyResult<Samples> {
    let mut s = Samples::default();
    for p in obj.getattr("points")?.try_iter()? {
        let p = p?;
        s.points.push((p.get_item(0)?.extract()?, p.get_item(1)?.extract()?));
    }
    s.pressures = obj.getattr("pressures")?.extract()?;
    s.timestamps = obj.getattr("timestamps")?.extract()?;
    s.tilts = obj.getattr("tilts")?.extract()?;
    s.orientations = obj.getattr("orientations")?.extract()?;
    Ok(s)
}


#[cfg(feature = "python")]
pub fn handle<'py>(py: Python<'py>, h: &Option<Handle>) -> PyResult<Bound<'py, PyAny>> {
    h.as_ref()
        .and_then(|b| b.downcast_ref::<Py<PyAny>>())
        .map(|o| o.bind(py).clone())
        .ok_or_else(|| PyRuntimeError::new_err("missing Python handle"))
}

#[cfg(feature = "python")]
pub fn any_obj<'py>(py: Python<'py>, a: &dyn Any) -> PyResult<Bound<'py, PyAny>> {
    a.downcast_ref::<Py<PyAny>>()
        .map(|o| o.bind(py).clone())
        .ok_or_else(|| PyRuntimeError::new_err("expected a Python object"))
}

#[cfg(feature = "python")]
pub fn snapshot_from(obj: &Bound<'_, PyAny>) -> PyResult<Snapshot> {
    Ok(Snapshot {
        raw_body: obj.getattr("raw_body")?.extract()?,
        content_type: obj.getattr("content_type")?.extract()?,
        resource_xml: obj.getattr("resource_xml")?.extract()?,
        local_id: obj.getattr("local_id")?.extract()?,
        commit_id: obj.getattr("commit_id")?.extract()?,
        handle: Some(Box::new(obj.clone().unbind())),
    })
}

#[cfg(feature = "python")]
/// Output/boundary classes owned by the ink area (rs-ink builds them as
/// Python dataclasses). Looked up like exception classes: no logic runs.
pub fn ink_class<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import(module)?.getattr(name)
}

#[cfg(feature = "python")]
pub fn ink_binary_part<'py>(py: Python<'py>, p: &BinaryPart) -> PyResult<Bound<'py, PyAny>> {
    let cls = ink_class(py, "samsung_notes.ink_editor", "InkBinaryPart")?;
    cls.call1((p.name.as_str(), p.filename.as_str(), PyBytes::new(py, &p.body)))
}

#[cfg(feature = "python")]
pub fn ink_append_edit<'py>(py: Python<'py>, args: Bound<'py, PyTuple>) -> PyResult<Bound<'py, PyAny>> {
    ink_class(py, "samsung_notes.ink_editor", "InkAppendEdit")?.call1(args)
}

#[cfg(feature = "python")]
pub fn stroke_samples_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    ink_class(py, "samsung_notes.ink_editor", "StrokeSamples")
}

#[cfg(feature = "python")]
pub fn normalized_capture_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    ink_class(py, "samsung_notes.ink_capture", "NormalizedCapture")
}

#[cfg(feature = "python")]
/// The doc area's `DocumentSnapshot` class (boundary type for isinstance /
/// from_dict; its logic lives in rs-doc).
pub fn document_snapshot_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("samsung_notes.document")?.getattr("DocumentSnapshot")
}

#[cfg(feature = "python")]
/// Python `PayloadResult` for the `verified_resource` return value only.
pub fn python_payload<'py>(py: Python<'py>, raw: &[u8], content_type: &str) -> PyResult<Bound<'py, PyAny>> {
    let limits = _doc::pyapi::payload_limits(py)?;
    let result = _doc::payload::parse_payload(raw, content_type, Some(crate::util::MAX_READONLY_PARTS as i64), &limits, &mut _doc::mime::NoHooks)
        .map_err(|e| to_py(py, from_doc(e)))?;
    let body = PyBytes::new(py, raw).into_any();
    let ct = pyo3::types::PyString::new(py, content_type).into_any();
    _doc::pyapi::payload_result_to_python(&body, &ct, &result)
}

fn from_doc(e: _doc::error::Error) -> Error {
    use _doc::error::Error as D;
    match e {
        D::Payload(m) => Error::new(Kind::Payload, m),
        D::Document(m) => Error::new(Kind::Doc, m.to_string()),
        D::Value(m) => Error::new(Kind::Value, m.to_string()),
        D::Type(m) => Error::new(Kind::Type, m.to_string()),
        D::XmlParse(m) => Error::new(Kind::Parse, m),
        D::Io(e) => Error::new(Kind::Other, e.to_string()),
        D::External(_) => Error::new(Kind::Other, "external error"),
    }
}

#[cfg(feature = "python")]
/// `tools.notes_payload.parse_payload(raw, ct, max_parts=MAX_READONLY_PARTS)` in rs-doc.
pub fn rust_payload(py: Python<'_>, raw: &[u8], content_type: &str) -> R<Payload> {
    let limits = _doc::pyapi::payload_limits(py).r(py)?;
    let p = _doc::payload::parse_payload(raw, content_type, Some(crate::util::MAX_READONLY_PARTS as i64), &limits, &mut _doc::mime::NoHooks)
        .map_err(from_doc)?;
    let body = |part: &_doc::mime::Part| raw[part.body.clone()].to_vec();
    Ok(Payload {
        locked: p.locked,
        xml_documents: p.xml_parts.iter().map(|i| body(&p.parts[*i])).collect(),
        parts: p
            .parts
            .iter()
            .map(|part| Part { name: part.name.clone(), filename: part.filename.clone(), content_type: part.content_type.clone(), body: body(part) })
            .collect(),
        handle: None,
    })
}

#[cfg(feature = "python")]
fn samples_py<'py>(py: Python<'py>, s: &Samples) -> PyResult<Bound<'py, PyAny>> {
    let pts: Vec<Bound<PyTuple>> = s.points.iter().map(|(x, y)| PyTuple::new(py, [*x, *y])).collect::<PyResult<_>>()?;
    stroke_samples_class(py)?.call1((
        PyTuple::new(py, pts)?,
        PyTuple::new(py, &s.pressures)?,
        PyTuple::new(py, &s.timestamps)?,
        PyTuple::new(py, &s.tilts)?,
        PyTuple::new(py, &s.orientations)?,
    ))
}

#[cfg(feature = "python")]
/// A decoded record as the ink area's Python `StrokeRecord` dataclass.
pub fn record_py<'py>(py: Python<'py>, r: &Record) -> PyResult<Bound<'py, PyAny>> {
    let ink = ink_record(r).ok_or_else(|| PyRuntimeError::new_err("record without rs-ink core data"))?;
    let frame_cls = ink_class(py, "samsung_notes.ink_editor", "StrokeFrame")?;
    let frames: Vec<Bound<PyAny>> = ink
        .frames
        .iter()
        .map(|f| {
            frame_cls.call1((
                PyBytes::new(py, &f.raw),
                f.kind,
                PyBytes::new(py, &f.properties),
                PyBytes::new(py, &f.fields),
                f.fixed_start,
                f.flexible_start,
            ))
        })
        .collect::<PyResult<_>>()?;
    ink_class(py, "samsung_notes.ink_editor", "StrokeRecord")?.call1((
        PyBytes::new(py, &ink.raw),
        PyTuple::new(py, frames)?,
        samples_py(py, &r.samples)?,
        ink.compressed,
        ink.stylus,
        ink.tool_type,
    ))
}

// ---------------------------------------------------------------- Deps impl

use _ink::core as inkcore;
use _ink::core::num::Val;

/// rs-ink error → ops error (Foreign = a Python error kept by the caller).
fn from_ink(e: inkcore::InkErr) -> Error {
    use inkcore::InkErr as I;
    match e {
        I::Ink(m) => Error::new(Kind::Ink, m),
        I::Document(m) => Error::new(Kind::Doc, m),
        I::Payload(m) => Error::new(Kind::Payload, m),
        I::Struct(m) => Error::new(Kind::Other, m),
        I::KeyError(m) => Error::new(Kind::Key, m),
        I::ValueError(m) => Error::new(Kind::Value, m),
        I::TypeError(m) => Error::new(Kind::Type, m),
        I::Overflow(m) => Error::new(Kind::Overflow, m),
        I::Parse(m) => Error::new(Kind::Parse, m),
        I::Unicode(enc, _, start, _, reason) => Error::new(Kind::Unicode, format!("'{enc}' codec can't decode byte in position {start}: {reason}")),
        I::Foreign => Error::new(Kind::Other, "foreign error"),
    }
}

fn ink_r<T>(r: inkcore::R<T>) -> R<T> {
    r.map_err(from_ink)
}

fn ink_record(r: &Record) -> Option<&inkcore::binary::Record> {
    r.handle.as_ref().and_then(|h| h.downcast_ref::<inkcore::binary::Record>())
}

fn core_record(r: &Record) -> R<&inkcore::binary::Record> {
    ink_record(r).ok_or_else(|| Error::new(Kind::Ink, "validated stroke record required"))
}

fn mask(b: &[u8]) -> u64 {
    b.iter().enumerate().take(8).fold(0u64, |a, (i, x)| a | (*x as u64) << (8 * i))
}

fn samples_of(s: &inkcore::binary::Samples) -> Samples {
    Samples {
        points: s.points.clone(),
        pressures: s.pressures.clone(),
        timestamps: s.timestamps.clone(),
        tilts: s.tilts.clone(),
        orientations: s.orientations.clone(),
    }
}

fn record_of(r: inkcore::binary::Record) -> Record {
    Record {
        raw: r.raw.clone(),
        frames: r
            .frames
            .iter()
            .map(|f| Frame {
                raw: f.raw.clone(),
                kind: f.kind,
                properties: mask(&f.properties),
                fields: mask(&f.fields),
                fixed_start: f.fixed_start.max(0) as usize,
                flexible_start: f.flexible_start.max(0) as usize,
            })
            .collect(),
        samples: samples_of(&r.samples),
        compressed: r.compressed,
        stylus: r.stylus,
        tool_type: r.tool_type as i64,
        handle: Some(Box::new(r)),
    }
}

fn style_of(s: &inkcore::binary::Style) -> Vec<(String, Num)> {
    s.0.iter()
        .filter_map(|(k, v)| match v {
            inkcore::binary::StyleVal::I(i) => Some((k.to_string(), Num::I(*i))),
            inkcore::binary::StyleVal::F(f) => Some((k.to_string(), Num::F(*f))),
            inkcore::binary::StyleVal::G(_) => None,
        })
        .collect()
}

/// Mirror an ops element subtree into an rs-ink tree.
/// `ink_cache.pdf_background(page)` over the rs-ink core.
pub fn pdf_background(page: &Element) -> R<Option<String>> {
    let mut tree = inkcore::xml::Tree::default();
    tree.root = to_tree(&mut tree, page, None);
    ink_r(inkcore::cache::pdf_background(&tree, tree.root))
}

fn to_tree(tree: &mut inkcore::xml::Tree, e: &Element, parent: Option<usize>) -> usize {
    let id = tree.add(&e.tag, e.attrib.clone(), parent);
    tree.nodes[id].text = e.text.clone();
    tree.nodes[id].tail = e.tail.clone();
    for c in &e.children {
        to_tree(tree, c, Some(id));
    }
    id
}

fn from_tree(tree: &inkcore::xml::Tree, id: usize) -> Element {
    let n = &tree.nodes[id];
    Element {
        tag: n.tag.clone(),
        attrib: n.attrs.clone(),
        text: n.text.clone(),
        tail: n.tail.clone(),
        children: n.children.iter().map(|c| from_tree(tree, *c)).collect(),
    }
}

#[cfg(feature = "python")]
fn val(v: &Bound<'_, PyAny>) -> Val {
    if v.is_exact_instance_of::<pyo3::types::PyInt>() {
        match v.extract::<i128>() {
            Ok(i) => Val::Int(i),
            Err(_) => Val::Big(v.lt(0i64).unwrap_or(false)),
        }
    } else if v.is_exact_instance_of::<pyo3::types::PyFloat>() {
        Val::Float(v.extract::<f64>().unwrap_or(f64::NAN))
    } else {
        Val::Bad
    }
}

#[cfg(feature = "python")]
fn raw_samples(obj: &Bound<'_, PyAny>) -> PyResult<inkcore::binary::RawSamples> {
    let vals = |name: &str| -> PyResult<Vec<Val>> { obj.getattr(name)?.try_iter()?.map(|v| Ok(val(&v?))).collect() };
    let mut points = Vec::new();
    for p in obj.getattr("points")?.try_iter()? {
        let p = p?;
        let pair = if p.is_instance_of::<PyTuple>() || p.is_instance_of::<PyList>() {
            let items: Vec<Bound<PyAny>> = p.try_iter()?.collect::<PyResult<_>>()?;
            if items.len() == 2 { Some([val(&items[0]), val(&items[1])]) } else { None }
        } else {
            None
        };
        points.push(pair);
    }
    Ok(inkcore::binary::RawSamples {
        points,
        pressures: vals("pressures")?,
        timestamps: vals("timestamps")?,
        tilts: vals("tilts")?,
        orientations: vals("orientations")?,
    })
}

#[cfg(feature = "python")]
pub struct PyDeps<'py> {
    pub py: Python<'py>,
}

#[cfg(feature = "python")]
impl Deps for PyDeps<'_> {
    #[cfg(feature = "python")]
    fn snapshot(&self, arg: &dyn Any) -> R<Option<Snapshot>> {
        let py = self.py;
        (|| -> PyResult<Option<Snapshot>> {
            let obj = any_obj(py, arg)?;
            let cls = document_snapshot_class(py)?;
            let obj = if obj.is_instance_of::<PyDict>() { cls.call_method1("from_dict", (obj,))? } else { obj };
            if !obj.is_instance(&cls)? {
                return Ok(None);
            }
            snapshot_from(&obj).map(Some)
        })()
        .r(py)
    }

    #[cfg(feature = "python")]
    fn parse_payload(&self, raw: &[u8], content_type: &str) -> R<Payload> {
        rust_payload(self.py, raw, content_type)
    }

    #[cfg(feature = "python")]
    fn convert(&self, arg: &dyn Any, module: &str) -> R<Handle> {
        let py = self.py;
        (|| -> PyResult<Handle> {
            let obj = any_obj(py, arg)?;
            let obj = if obj.is_instance_of::<PyDict>() {
                py.import(module)?.getattr("DocumentSnapshot")?.call_method1("from_dict", (obj,))?
            } else {
                obj
            };
            Ok(Box::new(obj.unbind()))
        })()
        .r(py)
    }

    #[cfg(feature = "python")]
    fn verified(&self, snapshot: &dyn Any) -> R<Option<String>> {
        let py = self.py;
        let f = py.import("samsung_notes.ink_projection").and_then(|m| m.getattr("verified_resource")).r(py)?;
        let own = VERIFIED_FN.get(py).is_some_and(|v| v.bind(py).is(&f));
        if own {
            return match crate::ink_projection::verified_resource(self, snapshot)? {
                crate::ink_projection::Verified::Reason(r) => Ok(Some(r.to_string())),
                _ => Ok(None),
            };
        }
        (|| -> PyResult<Option<String>> {
            let out = f.call1((any_obj(py, snapshot)?,))?;
            Ok(if out.is_instance_of::<pyo3::types::PyString>() { Some(out.extract()?) } else { None })
        })()
        .r(py)
    }

    #[cfg(feature = "python")]
    fn resource_xml(&self, snapshot: &dyn Any) -> R<Vec<u8>> {
        let py = self.py;
        (|| -> PyResult<Vec<u8>> { any_obj(py, snapshot)?.getattr("resource_xml")?.extract() })().r(py)
    }

    fn decode_stroke(&self, body: &[u8]) -> R<Record> {
        ink_r(inkcore::binary::decode_stroke_resource(body)).map(record_of)
    }

    fn bind(&self, record: &Record, object: &Element, strings: Option<&Element>) -> R<Binding> {
        let core = core_record(record)?;
        let mut tree = inkcore::xml::Tree::default();
        let obj = to_tree(&mut tree, object, None);
        let table = strings.map(|s| to_tree(&mut tree, s, None));
        let b = ink_r(inkcore::prepare::bind_stroke_metadata(&core.frames, (&tree, obj), table.map(|t| (&tree, t))))?;
        Ok(Binding {
            object_id: b.object_id,
            modified_time: b.modified_time,
            bounds: b.bounds,
            pen_name: b.pen_name,
            advanced_settings: b.advanced_settings,
            style: style_of(&b.style),
            handle: None,
        })
    }

    fn coordinate_scale(&self, record: &Record, width: i64, height: i64) -> R<f64> {
        let core = core_record(record)?;
        ink_r(inkcore::coords::page_coordinate_scale(&core.frames[0], Val::Int(width as i128), Val::Int(height as i128), Val::Int(0)))
    }

    fn owner_extent(&self, record: &Record) -> R<(i64, i64)> {
        ink_r(inkcore::coords::stroke_owner_extent(&core_record(record)?.frames[0]))
    }

    fn read_style(&self, record: &Record) -> R<Vec<(String, Num)>> {
        ink_r(inkcore::binary::read_stroke_style(&core_record(record)?.frames[1])).map(|s| style_of(&s))
    }

    fn decode_point_channels(&self, data: &[u8], count: usize, compressed: bool, stylus: bool) -> R<Samples> {
        ink_r(inkcore::binary::decode_point_channels(data, count, compressed, stylus)).map(|s| samples_of(&s))
    }

    fn release_page_cache(&self, root: &mut Element, page_id: &str, tick: i64) -> R<Vec<String>> {
        let mut tree = inkcore::xml::Tree::default();
        let r = to_tree(&mut tree, root, None);
        tree.root = r;
        let page = tree.nodes[r]
            .children
            .iter()
            .find(|c| crate::xml::local_name(&tree.nodes[**c].tag) == "pagelist")
            .and_then(|pl| tree.nodes[*pl].children.iter().find(|p| tree.get(**p, "id") == Some(page_id)).copied());
        let result = inkcore::cache::release_page_cache(&mut tree, page, tick as i128);
        *root = from_tree(&tree, r);
        ink_r(result)
    }

    #[cfg(feature = "python")]
    fn append_probe(&self, snapshot: &Snapshot, source: &str, new_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        let py = self.py;
        let raw = inkcore::binary::RawSamples {
            points: vec![Some([Val::Float(x), Val::Float(y)])],
            pressures: vec![Val::Float(0.5)],
            timestamps: vec![Val::Int(0)],
            tilts: vec![],
            orientations: vec![],
        };
        let capture = ink_r(inkcore::capture::normalize_capture(&raw, false))?;
        let foreign: std::cell::RefCell<Option<Error>> = std::cell::RefCell::new(None);
        let parse = |body: &[u8], ct: &str| -> inkcore::R<Vec<inkcore::prepare::PartIn>> {
            let parsed = rust_payload(py, body, ct).map_err(|e| {
                *foreign.borrow_mut() = Some(e);
                inkcore::InkErr::Foreign
            })?;
            Ok(parsed
                .parts
                .into_iter()
                .map(|p| inkcore::prepare::PartIn { name: p.name, filename: p.filename, content_type: p.content_type, body: p.body })
                .collect())
        };
        let snap = inkcore::prepare::SnapshotIn {
            resource_xml: &snapshot.resource_xml,
            raw_body: &snapshot.raw_body,
            content_type: &snapshot.content_type,
        };
        if snapshot.commit_id.as_deref().is_none_or(|c| c.is_empty()) {
            return Err(Error::new(Kind::Ink, "committed snapshot required"));
        }
        match inkcore::prepare::prepare_ink_document(
            &snap,
            Some(source),
            Some(new_id),
            inkcore::prepare::Geometry::Capture(&capture),
            Val::Int(now_ms as i128),
            2,
            None,
            &parse,
        ) {
            Ok(_) => Ok(()),
            Err(inkcore::InkErr::Foreign) => Err(foreign.borrow_mut().take().unwrap_or_else(|| Error::new(Kind::Other, "foreign error"))),
            Err(e) => Err(from_ink(e)),
        }
    }

    #[cfg(feature = "python")]
    fn first_stroke_probe(&self, snapshot: &Snapshot, page_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        let py = self.py;
        let samples = (|| -> PyResult<Py<PyAny>> {
            let cls = stroke_samples_class(py)?;
            let point = PyTuple::new(py, [PyTuple::new(py, [x, y])?])?;
            Ok(cls.call1((point, PyTuple::new(py, [0.5f64])?, PyTuple::new(py, [0i64])?))?.unbind())
        })()
        .r(py)?;
        let handle: &dyn Any = match snapshot.handle.as_deref() {
            Some(h) => h,
            None => return Err(Error::new(Kind::Ink, "snapshot required")),
        };
        crate::ink_first_stroke::prepare_first_stroke(
            self,
            handle,
            Some(page_id),
            Some(&samples as &dyn Any),
            Some(0xff00_0000),
            Some(1.0),
            Some(now_ms as i128),
            None,
        )
        .map(|_| ())
    }

    #[cfg(feature = "python")]
    fn highlighter_probe(&self, snapshot: &Snapshot, page_id: &str, x: f64, y: f64, now_ms: i64) -> R<()> {
        let py = self.py;
        let samples = (|| -> PyResult<Py<PyAny>> {
            let cls = stroke_samples_class(py)?;
            let point = PyTuple::new(py, [PyTuple::new(py, [x, y])?])?;
            Ok(cls.call1((point, PyTuple::new(py, [0.5f64])?, PyTuple::new(py, [0i64])?))?.unbind())
        })()
        .r(py)?;
        let handle: &dyn Any = match snapshot.handle.as_deref() {
            Some(h) => h,
            None => return Err(Error::new(Kind::Ink, "snapshot required")),
        };
        crate::ink_highlighter::prepare_highlighter_stroke(
            self,
            handle,
            Some(page_id),
            Some(&samples as &dyn Any),
            Some(crate::ink_highlighter::DEFAULT_COLOR as i128),
            Some(1.0),
            Some(now_ms as i128),
            None,
        )
        .map(|_| ())
    }

    fn basic_pens(&self) -> R<Vec<String>> {
        Ok(inkcore::capture::BASIC_PENS.iter().map(|s| s.to_string()).collect())
    }

    #[cfg(feature = "python")]
    fn normalize_capture(&self, samples: &dyn Any) -> R<Capture> {
        let py = self.py;
        let raw = (|| raw_samples(&any_obj(py, samples)?))().r(py)?;
        let c = ink_r(inkcore::capture::normalize_capture(&raw, false))?;
        Ok(Capture { samples: samples_of(&c.samples), channels: c.channels, stylus: c.stylus, handle: None })
    }

    fn drawn_bounds(&self, samples: &Samples, pen_name: &str, pen_size: f64) -> R<[f64; 4]> {
        let points: Vec<Option<[Val; 2]>> = samples.points.iter().map(|(x, y)| Some([Val::Float(*x), Val::Float(*y)])).collect();
        ink_r(inkcore::capture::basic_pen_drawn_bounds(Some(&points), Some(pen_name), Val::Float(pen_size), Val::Int(0)))
    }
}

#[cfg(feature = "python")]
pub fn capture_from(cap: &Bound<'_, PyAny>) -> PyResult<Capture> {
    Ok(Capture {
        samples: samples_from(&cap.getattr("samples")?)?,
        channels: cap.getattr("channels")?.extract()?,
        stylus: cap.getattr("stylus")?.is_truthy()?,
        handle: Some(Box::new(cap.clone().unbind())),
    })
}

#[cfg(feature = "python")]
/// Box a Python argument as the opaque `&dyn Any` the core passes back to Deps.
pub fn opaque(obj: &Bound<'_, PyAny>) -> Py<PyAny> {
    obj.clone().unbind()
}

#[cfg(feature = "python")]
pub fn tuple_str<'py>(py: Python<'py>, items: &[String]) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items)
}

#[cfg(feature = "python")]
pub fn frozenset_str<'py>(py: Python<'py>, items: &[String]) -> PyResult<Bound<'py, PyAny>> {
    Ok(pyo3::types::PyFrozenSet::new(py, items)?.into_any())
}
