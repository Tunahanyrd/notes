//! Python <-> core conversions shared by the module wrappers.
use crate::core::binary::{Frame, RawSamples, Record, Samples, Style, StyleVal};
use crate::core::num::Val;
use crate::core::xml::{Op, Tree, NON_STR_TAG};
use crate::core::InkErr;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyIndexError, PyKeyError, PyOverflowError, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::sync::PyOnceLock;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use std::collections::HashMap;

// ---- exceptions -----------------------------------------------------------

#[cfg(feature = "python")]
static INK_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();

#[cfg(feature = "python")]
pub fn ink_error(py: Python<'_>) -> &Bound<'_, PyType> {
    INK_ERROR
        .get_or_init(py, || {
            PyErr::new_type(py, c"samsung_notes.ink.InkError", None, Some(&py.get_type::<PyValueError>()), None)
                .expect("InkError type")
        })
        .bind(py)
}

#[cfg(feature = "python")]
fn class_from(py: Python<'_>, module: &str, name: &str) -> PyResult<Py<PyAny>> {
    Ok(py.import(module)?.getattr(name)?.unbind())
}

#[cfg(feature = "python")]
/// Convert a core error into the Python exception class the oracle raises.
pub fn err(py: Python<'_>, e: InkErr) -> PyErr {
    let raise = |module: &str, name: &str, msg: String| match class_from(py, module, name) {
        Ok(cls) => match cls.bind(py).call1((msg,)) {
            Ok(inst) => PyErr::from_value(inst),
            Err(e) => e,
        },
        Err(e) => e,
    };
    match e {
        InkErr::Ink(m) => match ink_error(py).call1((m,)) {
            Ok(inst) => PyErr::from_value(inst),
            Err(e) => e,
        },
        InkErr::Document(m) => rs_doc::pyapi::document_error(py, &m),
        InkErr::Payload(m) => rs_doc::pyapi::payload_error(py, &m),
        InkErr::Struct(m) => raise("struct", "error", m),
        InkErr::Parse(m) => raise("xml.etree.ElementTree", "ParseError", m),
        InkErr::KeyError(m) => PyKeyError::new_err(m),
        InkErr::ValueError(m) => PyValueError::new_err(m),
        InkErr::TypeError(m) => PyTypeError::new_err(m),
        InkErr::Overflow(m) => PyOverflowError::new_err(m),
        InkErr::Unicode(enc, data, start, end, reason) => {
            match py.get_type::<pyo3::exceptions::PyUnicodeDecodeError>()
                .call1((enc, PyBytes::new(py, &data), start, end, reason)) {
                Ok(inst) => PyErr::from_value(inst),
                Err(e) => e,
            }
        }
        InkErr::Foreign => PyValueError::new_err("foreign error lost"),
    }
}

pub trait IntoPy<T> {
    #[cfg(feature = "python")]
    fn py(self, py: Python<'_>) -> PyResult<T>;
}
impl<T> IntoPy<T> for Result<T, InkErr> {
    #[cfg(feature = "python")]
    fn py(self, py: Python<'_>) -> PyResult<T> {
        self.map_err(|e| err(py, e))
    }
}

// ---- dataclasses ----------------------------------------------------------

#[cfg(feature = "python")]
struct FieldSpec {
    name: &'static str,
    repr: bool,
    default: Option<fn(Python<'_>) -> Py<PyAny>>,
}

#[cfg(feature = "python")]
const fn f(name: &'static str) -> FieldSpec {
    FieldSpec { name, repr: true, default: None }
}

#[cfg(feature = "python")]
fn empty_tuple(py: Python<'_>) -> Py<PyAny> {
    PyTuple::empty(py).into_any().unbind()
}
#[cfg(feature = "python")]
fn octet(py: Python<'_>) -> Py<PyAny> {
    PyString::new(py, "application/octet-stream").into_any().unbind()
}
#[cfg(feature = "python")]
fn one_int(py: Python<'_>) -> Py<PyAny> {
    1i64.into_pyobject(py).unwrap().into_any().unbind()
}

#[cfg(feature = "python")]
struct ClassSpec {
    name: &'static str,
    module: &'static str,
    repr: bool,
    fields: &'static [FieldSpec],
}

#[cfg(feature = "python")]
const CLASSES: &[ClassSpec] = &[
    ClassSpec { name: "CanvasPreviewCandidate", module: "samsung_notes.ink", repr: true, fields: &[
        f("page_index"), f("key"), f("width"), f("height"), f("is_dark_mode"),
        FieldSpec { name: "bind_number", repr: false, default: None },
        FieldSpec { name: "sha256", repr: false, default: None },
        FieldSpec { name: "download_url", repr: false, default: None },
    ] },
    ClassSpec { name: "InkInventory", module: "samsung_notes.ink", repr: true, fields: &[
        f("stroke_parts"), f("wrapped_v4000_parts"), f("page_count"), f("canvas_candidates"),
        f("unresolved_canvas_references"),
    ] },
    ClassSpec { name: "SpiHeader", module: "samsung_notes.ink", repr: true, fields: &[
        f("width"), f("height"), f("color_index"), f("row_group_size"), f("reference_cache_capacity"), f("flags"),
        f("encoded_data_bytes"),
    ] },
    ClassSpec { name: "StrokeSamples", module: "samsung_notes.ink_editor", repr: false, fields: &[
        f("points"), f("pressures"), f("timestamps"),
        FieldSpec { name: "tilts", repr: true, default: Some(empty_tuple) },
        FieldSpec { name: "orientations", repr: true, default: Some(empty_tuple) },
    ] },
    ClassSpec { name: "StrokeFrame", module: "samsung_notes.ink_editor", repr: false, fields: &[
        f("raw"), f("kind"), f("properties"), f("fields"), f("fixed_start"), f("flexible_start"),
    ] },
    ClassSpec { name: "StrokeRecord", module: "samsung_notes.ink_editor", repr: false, fields: &[
        f("raw"), f("frames"), f("samples"), f("compressed"), f("stylus"), f("tool_type"),
    ] },
    ClassSpec { name: "StrokeBinding", module: "samsung_notes.ink_editor", repr: false, fields: &[
        f("object_id"), f("modified_time"), f("bounds"), f("pen_name"), f("advanced_settings"), f("style"),
    ] },
    ClassSpec { name: "InkBinaryPart", module: "samsung_notes.ink_editor", repr: false, fields: &[
        f("name"), f("filename"), f("body"),
        FieldSpec { name: "content_type", repr: true, default: Some(octet) },
    ] },
    ClassSpec { name: "InkAppendEdit", module: "samsung_notes.ink_editor", repr: false, fields: &[
        f("full_xml"), f("tree_xml"), f("upload_xml"), f("requested_ids"), f("page_id"), f("layer_id"),
        f("stroke_id"), f("stroke_modified_time"), f("binary_parts"), f("title"), f("body"),
        FieldSpec { name: "content_file_ids", repr: true, default: Some(empty_tuple) },
        FieldSpec { name: "recipe_version", repr: true, default: Some(one_int) },
    ] },
    ClassSpec { name: "NormalizedCapture", module: "samsung_notes.ink_capture", repr: false, fields: &[
        f("samples"), f("channels"), f("stylus"), f("max_coordinate_error"), f("max_pressure_error"),
    ] },
];

#[cfg(feature = "python")]
static TYPES: PyOnceLock<HashMap<&'static str, Py<PyType>>> = PyOnceLock::new();

#[cfg(feature = "python")]
fn build_types(py: Python<'_>) -> PyResult<HashMap<&'static str, Py<PyType>>> {
    let dc = py.import("dataclasses")?;
    let make = dc.getattr("make_dataclass")?;
    let field = dc.getattr("field")?;
    let object = py.get_type::<pyo3::types::PyAny>();
    let mut out = HashMap::new();
    for spec in CLASSES {
        let fields = PyList::empty(py);
        for fs in spec.fields {
            let kw = PyDict::new(py);
            if !fs.repr {
                kw.set_item("repr", false)?;
            }
            if let Some(d) = fs.default {
                kw.set_item("default", d(py))?;
            }
            if kw.is_empty() {
                fields.append((fs.name, &object))?;
            } else {
                fields.append((fs.name, &object, field.call((), Some(&kw))?))?;
            }
        }
        let kw = PyDict::new(py);
        kw.set_item("frozen", true)?;
        kw.set_item("repr", spec.repr)?;
        kw.set_item("module", spec.module)?;
        let cls = make.call((spec.name, fields), Some(&kw))?;
        out.insert(spec.name, cls.cast_into::<PyType>()?.unbind());
    }
    Ok(out)
}

#[cfg(feature = "python")]
pub fn cls<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyType>> {
    let types = TYPES.get_or_try_init(py, || build_types(py))?;
    Ok(types[name].bind(py).clone())
}

#[cfg(feature = "python")]
pub fn add_classes(m: &Bound<'_, PyModule>, names: &[&str]) -> PyResult<()> {
    for n in names {
        m.add(*n, cls(m.py(), n)?)?;
    }
    Ok(())
}

#[cfg(feature = "python")]
pub fn is_inst(obj: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    obj.is_instance(cls(obj.py(), name)?.as_any())
}

// ---- values ---------------------------------------------------------------

#[cfg(feature = "python")]
/// Classify a Python object like `type(v) in (int, float)` checks do.
pub fn val(v: &Bound<'_, PyAny>) -> Val {
    if v.is_exact_instance_of::<PyInt>() {
        match v.extract::<i128>() {
            Ok(i) => Val::Int(i),
            Err(_) => Val::Big(v.lt(0i64).unwrap_or(false)),
        }
    } else if v.is_exact_instance_of::<PyFloat>() {
        Val::Float(v.extract::<f64>().unwrap_or(f64::NAN))
    } else {
        Val::Bad
    }
}

#[cfg(feature = "python")]
pub fn bytes_of(v: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = v.cast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if let Ok(b) = v.cast::<pyo3::types::PyByteArray>() {
        return Ok(b.to_vec());
    }
    Err(PyTypeError::new_err("a bytes-like object is required"))
}

#[cfg(feature = "python")]
fn seq_items<'py>(v: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    if let Ok(t) = v.cast::<PyTuple>() {
        return Ok(t.iter().collect());
    }
    if let Ok(l) = v.cast::<PyList>() {
        return Ok(l.iter().collect());
    }
    v.try_iter()?.collect()
}

#[cfg(feature = "python")]
/// A StrokeSamples-like object as RawSamples.
pub fn raw_samples(s: &Bound<'_, PyAny>) -> PyResult<RawSamples> {
    let vals = |name: &str| -> PyResult<Vec<Val>> { Ok(seq_items(&s.getattr(name)?)?.iter().map(val).collect()) };
    let mut points = Vec::new();
    for p in seq_items(&s.getattr("points")?)? {
        let pair = if p.is_instance_of::<PyTuple>() || p.is_instance_of::<PyList>() {
            let items = seq_items(&p)?;
            if items.len() == 2 { Some([val(&items[0]), val(&items[1])]) } else { None }
        } else {
            None
        };
        points.push(pair);
    }
    Ok(RawSamples { points, pressures: vals("pressures")?, timestamps: vals("timestamps")?, tilts: vals("tilts")?,
                    orientations: vals("orientations")? })
}

#[cfg(feature = "python")]
pub fn samples_obj<'py>(py: Python<'py>, s: &Samples) -> PyResult<Bound<'py, PyAny>> {
    let points = PyTuple::new(py, s.points.iter().map(|(x, y)| PyTuple::new(py, [*x, *y]).unwrap()))?;
    let floats = |v: &[f64]| PyTuple::new(py, v.iter().copied());
    cls(py, "StrokeSamples")?.call1((
        points,
        floats(&s.pressures)?,
        PyTuple::new(py, s.timestamps.iter().copied())?,
        floats(&s.tilts)?,
        floats(&s.orientations)?,
    ))
}

#[cfg(feature = "python")]
pub fn frame_obj<'py>(py: Python<'py>, fr: &Frame) -> PyResult<Bound<'py, PyAny>> {
    cls(py, "StrokeFrame")?.call1((
        PyBytes::new(py, &fr.raw),
        fr.kind,
        PyBytes::new(py, &fr.properties),
        PyBytes::new(py, &fr.fields),
        fr.fixed_start,
        fr.flexible_start,
    ))
}

#[cfg(feature = "python")]
pub fn record_obj<'py>(py: Python<'py>, r: &Record) -> PyResult<Bound<'py, PyAny>> {
    let frames = PyTuple::new(py, r.frames.iter().map(|f| frame_obj(py, f)).collect::<PyResult<Vec<_>>>()?)?;
    cls(py, "StrokeRecord")?.call1((
        PyBytes::new(py, &r.raw),
        frames,
        samples_obj(py, &r.samples)?,
        r.compressed,
        r.stylus,
        r.tool_type,
    ))
}

#[cfg(feature = "python")]
pub fn frame_from(obj: &Bound<'_, PyAny>) -> PyResult<Frame> {
    Ok(Frame {
        raw: bytes_of(&obj.getattr("raw")?)?,
        kind: obj.getattr("kind")?.extract()?,
        properties: bytes_of(&obj.getattr("properties")?)?,
        fields: bytes_of(&obj.getattr("fields")?)?,
        fixed_start: obj.getattr("fixed_start")?.extract()?,
        flexible_start: obj.getattr("flexible_start")?.extract()?,
    })
}

#[cfg(feature = "python")]
/// `record.frames[i]` for the requested indices, converted.
pub fn record_frames(record: &Bound<'_, PyAny>, needed: usize) -> PyResult<Vec<Frame>> {
    let frames = record.getattr("frames")?;
    let mut out = Vec::new();
    for i in 0..needed {
        let item = frames.get_item(i).map_err(|_| PyIndexError::new_err("tuple index out of range"))?;
        out.push(frame_from(&item)?);
    }
    Ok(out)
}

#[cfg(feature = "python")]
pub fn style_obj<'py>(py: Python<'py>, s: &Style) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in &s.0 {
        match v {
            StyleVal::I(i) => d.set_item(*k, *i)?,
            StyleVal::F(f) => d.set_item(*k, *f)?,
            StyleVal::G(g) => d.set_item(*k, PyTuple::new(py, g.iter().copied())?)?,
        }
    }
    Ok(d)
}

// ---- ElementTree bridge ---------------------------------------------------

#[cfg(feature = "python")]
fn opt_str(v: Bound<'_, PyAny>) -> PyResult<Option<String>> {
    if v.is_none() {
        Ok(None)
    } else {
        Ok(Some(v.str()?.to_cow()?.into_owned()))
    }
}

#[cfg(feature = "python")]
/// Mirror a Python ElementTree subtree; `objs[id]` is the Python element of node `id`.
pub fn tree_from<'py>(elem: &Bound<'py, PyAny>, with_tail: bool) -> PyResult<(Tree, Vec<Bound<'py, PyAny>>)> {
    let mut tree = Tree::default();
    let mut objs = Vec::new();
    #[cfg(feature = "python")]
    fn walk<'py>(tree: &mut Tree, objs: &mut Vec<Bound<'py, PyAny>>, e: &Bound<'py, PyAny>, parent: Option<usize>,
                 with_tail: bool) -> PyResult<usize> {
        let tag = e.getattr("tag")?;
        let tag = match tag.cast::<PyString>() {
            Ok(s) => s.to_cow()?.into_owned(),
            Err(_) => NON_STR_TAG.to_string(),
        };
        let attrib = e.getattr("attrib")?;
        let mut attrs = Vec::new();
        if let Ok(d) = attrib.cast::<PyDict>() {
            for (k, v) in d.iter() {
                let k = k.str()?.to_cow()?.into_owned();
                let v = match v.cast::<PyString>() {
                    Ok(s) => s.to_cow()?.into_owned(),
                    Err(_) => v.str()?.to_cow()?.into_owned(),
                };
                attrs.push((k, v));
            }
        }
        let id = tree.add(&tag, attrs, parent);
        tree.nodes[id].text = opt_str(e.getattr("text")?)?;
        if with_tail {
            tree.nodes[id].tail = opt_str(e.getattr("tail")?)?;
        }
        objs.push(e.clone());
        for child in e.try_iter()? {
            walk(tree, objs, &child?, Some(id), with_tail)?;
        }
        Ok(id)
    }
    let root = walk(&mut tree, &mut objs, elem, None, with_tail)?;
    tree.root = root;
    Ok((tree, objs))
}

#[cfg(feature = "python")]
/// Replay logged DOM mutations on the Python elements.
pub fn replay(objs: &[Bound<'_, PyAny>], ops: &[Op]) -> PyResult<()> {
    for op in ops {
        match op {
            Op::Set(id, k, v) => {
                objs[*id].call_method1("set", (k, v))?;
            }
            Op::Remove(p, c) => {
                objs[*p].call_method1("remove", (&objs[*c],))?;
            }
        }
    }
    Ok(())
}
