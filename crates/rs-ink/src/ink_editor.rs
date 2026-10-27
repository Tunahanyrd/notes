//! Replacement for the Python module `ink_editor`.
use crate::core::binary;
use crate::core::capture::NormalizedCapture;
use crate::core::num::Val;
use crate::core::prepare::{self, Geometry, SnapshotIn, StyleOverride};
use crate::core::xml::Tree;
use crate::core::InkErr;
#[cfg(feature = "python")]
use crate::pyglue::{bytes_of, cls, err, is_inst, raw_samples, record_frames, record_obj, samples_obj, style_obj,
                    tree_from, val, IntoPy};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBool, PyBytes, PyFrozenSet, PyString, PyTuple};

#[cfg(feature = "python")]
fn ink_err(py: Python<'_>, msg: &str) -> PyErr {
    err(py, InkErr::Ink(msg.into()))
}

#[cfg(feature = "python")]
fn require_record(py: Python<'_>, record: &Bound<'_, PyAny>) -> PyResult<()> {
    if !is_inst(record, "StrokeRecord")? {
        return Err(ink_err(py, "validated stroke record required"));
    }
    Ok(())
}

#[cfg(feature = "python")]
#[pyfunction]
fn read_stroke_style<'py>(py: Python<'py>, record: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    require_record(py, record)?;
    let frames = record_frames(record, 2)?;
    Ok(style_obj(py, &binary::read_stroke_style(&frames[1]).py(py)?)?.into_any())
}

#[cfg(feature = "python")]
/// Shallow mirror of one element (tag + attributes only).
fn element_shallow(e: &Bound<'_, PyAny>) -> PyResult<Tree> {
    let mut t = Tree::default();
    let tag = e.getattr("tag")?;
    let tag = match tag.cast::<PyString>() {
        Ok(s) => s.to_cow()?.into_owned(),
        Err(_) => crate::core::xml::NON_STR_TAG.to_string(),
    };
    let mut attrs = Vec::new();
    for item in e.call_method0("items")?.try_iter()? {
        let (k, v): (Bound<PyAny>, Bound<PyAny>) = item?.extract()?;
        attrs.push((k.str()?.to_cow()?.into_owned(), v.str()?.to_cow()?.into_owned()));
    }
    t.root = t.add(&tag, attrs, None);
    Ok(t)
}

#[cfg(feature = "python")]
pub fn binding_obj<'py>(py: Python<'py>, b: &prepare::Binding) -> PyResult<Bound<'py, PyAny>> {
    cls(py, "StrokeBinding")?.call1((
        &b.object_id,
        b.modified_time,
        PyTuple::new(py, b.bounds)?,
        &b.pen_name,
        b.advanced_settings.as_deref(),
        style_obj(py, &b.style)?,
    ))
}

#[cfg(feature = "python")]
#[pyfunction]
fn bind_stroke_metadata<'py>(py: Python<'py>, record: &Bound<'py, PyAny>, xml_object: &Bound<'py, PyAny>,
                             string_id_list: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    require_record(py, record)?;
    let frames = record_frames(record, 2)?;
    let obj = element_shallow(xml_object)?;
    let table = if string_id_list.is_none() { None } else { Some(tree_from(string_id_list, false)?.0) };
    let b = prepare::bind_stroke_metadata(&frames, (&obj, obj.root), table.as_ref().map(|t| (t, t.root))).py(py)?;
    binding_obj(py, &b)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (data, count, *, compressed, stylus))]
fn decode_point_channels<'py>(py: Python<'py>, data: &Bound<'py, PyAny>, count: &Bound<'py, PyAny>,
                              compressed: &Bound<'py, PyAny>, stylus: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let n = match val(count) {
        Val::Int(i) if (0..=binary::MAX_POINTS as i128).contains(&i) => i as usize,
        _ => return Err(ink_err(py, "invalid channel profile")),
    };
    if !compressed.is_exact_instance_of::<PyBool>() || !stylus.is_exact_instance_of::<PyBool>() {
        return Err(ink_err(py, "invalid channel profile"));
    }
    let Ok(b) = data.cast::<PyBytes>() else {
        return Err(ink_err(py, "stroke channel length mismatch"));
    };
    let s = binary::decode_point_channels(b.as_bytes(), n, compressed.is_truthy()?, stylus.is_truthy()?).py(py)?;
    samples_obj(py, &s)
}

#[cfg(feature = "python")]
fn encode_checked<'py>(py: Python<'py>, samples: &Bound<'py, PyAny>, compressed: bool, stylus: bool) -> PyResult<Vec<u8>> {
    let raw = raw_samples(samples)?;
    let encoded = binary::encode_point_channels_raw(&raw, compressed, stylus).py(py)?;
    let decoded = binary::decode_point_channels(&encoded, raw.points.len(), compressed, stylus).py(py)?;
    if samples_obj(py, &decoded)?.ne(samples)? {
        return Err(ink_err(py, "native float accumulation changes requested samples"));
    }
    Ok(encoded)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (samples, *, compressed, stylus))]
fn encode_point_channels<'py>(py: Python<'py>, samples: &Bound<'py, PyAny>, compressed: &Bound<'py, PyAny>,
                              stylus: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    if !is_inst(samples, "StrokeSamples")? || !compressed.is_exact_instance_of::<PyBool>()
        || !stylus.is_exact_instance_of::<PyBool>()
    {
        return Err(ink_err(py, "invalid stroke samples"));
    }
    let out = encode_checked(py, samples, compressed.is_truthy()?, stylus.is_truthy()?)?;
    Ok(PyBytes::new(py, &out))
}

#[cfg(feature = "python")]
#[pyfunction]
fn decode_stroke_resource<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let Ok(b) = raw.cast::<PyBytes>() else {
        return Err(ink_err(py, "stroke resource exceeds bounds"));
    };
    record_obj(py, &binary::decode_stroke_resource(b.as_bytes()).py(py)?)
}

#[cfg(feature = "python")]
#[pyfunction]
fn replace_stroke_channels_offline<'py>(py: Python<'py>, record: &Bound<'py, PyAny>, samples: &Bound<'py, PyAny>)
                                       -> PyResult<Bound<'py, PyBytes>> {
    require_record(py, record)?;
    let raw_obj = record.getattr("raw")?;
    let Ok(raw) = raw_obj.cast::<PyBytes>() else {
        return Err(ink_err(py, "stroke resource exceeds bounds"));
    };
    let checked = binary::decode_stroke_resource(raw.as_bytes()).py(py)?;
    if record_obj(py, &checked)?.ne(record)? {
        return Err(ink_err(py, "stroke record no longer matches original bytes"));
    }
    if samples.eq(record.getattr("samples")?)? {
        return Ok(raw.clone());
    }
    if !is_inst(samples, "StrokeSamples")? {
        return Err(ink_err(py, "invalid stroke samples"));
    }
    let channels = encode_checked(py, samples, checked.compressed, checked.stylus)?;
    let count = samples.getattr("points")?.len()?;
    let result = binary::rebuild_with_channels(&checked, count, &channels).py(py)?;
    let rebuilt = binary::decode_stroke_resource(&result).py(py)?;
    if samples_obj(py, &rebuilt.samples)?.ne(samples)? {
        return Err(ink_err(py, "stroke reconstruction mismatch"));
    }
    Ok(PyBytes::new(py, &result))
}

#[cfg(feature = "python")]
fn opt_str(v: &Bound<'_, PyAny>) -> Option<String> {
    v.cast::<PyString>().ok().and_then(|s| s.to_str().ok().map(str::to_string))
}

#[cfg(feature = "python")]
fn bytes_or_str(v: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    match v.cast::<PyString>() {
        Ok(s) => Ok(s.to_str()?.as_bytes().to_vec()),
        Err(_) => bytes_of(v),
    }
}

pub enum GeomIn<'a> {
    Translate(Val, Val),
    Capture(&'a NormalizedCapture),
}

#[cfg(feature = "python")]
/// Python wrapper around core::prepare::prepare_ink_document.
#[allow(clippy::too_many_arguments)]
pub fn prepare_py<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>,
                       new_stroke_id: &Bound<'py, PyAny>, geometry: GeomIn, now_ms: &Bound<'py, PyAny>,
                       recipe_version: u8, style: Option<StyleOverride>) -> PyResult<Bound<'py, PyAny>> {
    let mut snapshot = snapshot.clone();
    // Python-visible snapshot objects come from samsung_notes.document (rs-doc's own types).
    let doc_cls = py.import("samsung_notes.document")?.getattr("DocumentSnapshot")?;
    if snapshot.is_instance_of::<pyo3::types::PyDict>() {
        snapshot = doc_cls.call_method1("from_dict", (snapshot,))?;
    }
    if !snapshot.is_instance(&doc_cls)? || !snapshot.getattr("commit_id")?.is_truthy()? {
        return Err(ink_err(py, "committed snapshot required"));
    }
    let resource_xml = bytes_or_str(&snapshot.getattr("resource_xml")?)?;
    let raw_body = bytes_of(&snapshot.getattr("raw_body")?)?;
    let content_type: String = snapshot.getattr("content_type")?.extract()?;
    let limits = rs_doc::pyapi::payload_limits(py)?;
    let parse = |raw: &[u8], ct: &str| prepare::payload_parts(raw, ct, &limits);
    let snap = SnapshotIn { resource_xml: &resource_xml, raw_body: &raw_body, content_type: &content_type };
    let geom = match geometry {
        GeomIn::Translate(dx, dy) => Geometry::Translate(dx, dy),
        GeomIn::Capture(c) => Geometry::Capture(c),
    };
    let result = prepare::prepare_ink_document(&snap, opt_str(source_stroke_id).as_deref(), opt_str(new_stroke_id).as_deref(),
                                               geom, val(now_ms), recipe_version, style.as_ref(), &parse);
    let edit = result.py(py)?;
    let part_cls = cls(py, "InkBinaryPart")?;
    let parts: Vec<Bound<PyAny>> = edit.binary_parts.iter()
        .map(|(n, f, b)| part_cls.call1((n, f, PyBytes::new(py, b))))
        .collect::<PyResult<_>>()?;
    let args: Vec<Bound<PyAny>> = vec![
        PyBytes::new(py, &edit.full_xml).into_any(),
        PyBytes::new(py, &edit.tree_xml).into_any(),
        PyBytes::new(py, &edit.upload_xml).into_any(),
        PyFrozenSet::new(py, &edit.requested_ids)?.into_any(),
        PyString::new(py, &edit.page_id).into_any(),
        PyString::new(py, &edit.layer_id).into_any(),
        PyString::new(py, &edit.stroke_id).into_any(),
        edit.stroke_modified_time.into_pyobject(py)?.into_any(),
        PyTuple::new(py, parts)?.into_any(),
        snapshot.getattr("title")?,
        snapshot.getattr("body")?,
        PyTuple::new(py, &edit.content_file_ids)?.into_any(),
        edit.recipe_version.into_pyobject(py)?.into_any(),
    ];
    cls(py, "InkAppendEdit")?.call1(PyTuple::new(py, args)?)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (snapshot, source_stroke_id, *, new_stroke_id, dx, dy, now_ms))]
fn prepare_ink_clone<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, source_stroke_id: &Bound<'py, PyAny>,
                          new_stroke_id: &Bound<'py, PyAny>, dx: &Bound<'py, PyAny>, dy: &Bound<'py, PyAny>,
                          now_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    prepare_py(py, snapshot, source_stroke_id, new_stroke_id, GeomIn::Translate(val(dx), val(dy)), now_ms, 1, None)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::pyglue::add_classes(m, &["StrokeSamples", "StrokeFrame", "StrokeRecord", "StrokeBinding", "InkBinaryPart",
                                    "InkAppendEdit"])?;
    m.add_function(wrap_pyfunction!(read_stroke_style, m)?)?;
    m.add_function(wrap_pyfunction!(bind_stroke_metadata, m)?)?;
    m.add_function(wrap_pyfunction!(decode_point_channels, m)?)?;
    m.add_function(wrap_pyfunction!(encode_point_channels, m)?)?;
    m.add_function(wrap_pyfunction!(decode_stroke_resource, m)?)?;
    m.add_function(wrap_pyfunction!(replace_stroke_channels_offline, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_ink_clone, m)?)?;
    rs_common::export(m, &["StrokeSamples", "StrokeFrame", "StrokeRecord", "StrokeBinding", "InkBinaryPart",
                           "InkAppendEdit", "read_stroke_style", "bind_stroke_metadata", "decode_point_channels",
                           "encode_point_channels", "decode_stroke_resource", "replace_stroke_channels_offline",
                           "prepare_ink_clone"])
}
