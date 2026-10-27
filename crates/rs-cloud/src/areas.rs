//! Calls into the STABLE area crates. Pure data paths use the rlibs
//! (rs-doc, rs-editor, rs-ops cores); Python-facing objects (DocumentSnapshot,
//! StrokeSamples, editor models, ink edits) go through the area's Python module,
//! which is that area's Rust implementation under NOTES_IMPL=rust (`live`).
//! Exception classes are always the live ones (never this cdylib's copies).
use crate::glue::*;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyAttributeError, PyOverflowError, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyFrozenSet, PyString, PyTuple};
use rs_doc::mime::NoHooks;

#[cfg(feature = "python")]
/// A Python-facing object/class of a STABLE area, looked up at call time.
pub fn live<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import(module)?.getattr(name)
}

#[cfg(feature = "python")]
fn raise(py: Python<'_>, module: &str, class: &str, message: String) -> PyErr {
    match py.import(module).and_then(|m| m.getattr(class)).and_then(|c| c.call1((message,))) {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
/// rs-doc error -> the live Python exception.
pub fn doc_err(py: Python<'_>, e: rs_doc::Error) -> PyErr {
    use rs_doc::Error as E;
    match e {
        E::Payload(m) => rs_doc::pyapi::payload_error(py, m),
        E::Document(m) => rs_doc::pyapi::document_error(py, &m),
        E::Value(m) => PyValueError::new_err(m.into_owned()),
        E::Type(m) => PyTypeError::new_err(m.into_owned()),
        E::XmlParse(m) => raise(py, "xml.etree.ElementTree", "ParseError", m),
        E::Io(io) => PyErr::from(io),
        E::External(b) => match b.downcast::<PyErr>() {
            Ok(err) => *err,
            Err(_) => PyValueError::new_err("external error"),
        },
    }
}

#[cfg(feature = "python")]
/// rs-editor error -> the live Python exception.
pub fn editor_err(py: Python<'_>, e: rs_editor::rich::Error) -> PyErr {
    use rs_editor::rich::Error as E;
    match e {
        E::Document(m) => rs_doc::pyapi::document_error(py, &m),
        E::Value(m) => PyValueError::new_err(m),
        E::Type(m) => PyTypeError::new_err(m),
        E::Overflow(m) => PyOverflowError::new_err(m),
        E::Attribute(m) => PyAttributeError::new_err(m),
        E::Parse(m) => raise(py, "xml.etree.ElementTree", "ParseError", m),
        E::Payload(m) => rs_doc::pyapi::payload_error(py, &m),
        E::External { payload, message, .. } => match payload.downcast::<PyErr>() {
            Ok(err) => *err,
            Err(_) => PyValueError::new_err(message),
        },
    }
}

#[cfg(feature = "python")]
/// rs-ops error -> the live Python exception.
pub fn ops_err(py: Python<'_>, e: rs_ops::err::Error) -> PyErr {
    rs_ops::bridge::to_py(py, e)
}

// ------------------------------------------------------------ document

#[cfg(feature = "python")]
pub fn doc_limits(py: Python<'_>) -> PyResult<rs_doc::doc::DocLimits> {
    rs_doc::pyapi::doc_limits(py)
}

#[cfg(feature = "python")]
pub fn max_body_bytes(py: Python<'_>) -> PyResult<usize> {
    Ok(doc_limits(py)?.max_body_bytes)
}

#[cfg(feature = "python")]
/// document.text_value
pub fn text_value(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<()> {
    let invalid = || rs_doc::pyapi::document_error(py, "Metin geçersiz veya desteklenen boyutu aşıyor.");
    let s = value.cast::<PyString>().map_err(|_| invalid())?;
    let s = s.to_str().map_err(|_| invalid())?;
    rs_doc::doc::text_value(s, doc_limits(py)?.max_text).map_err(|_| invalid())
}

#[cfg(feature = "python")]
/// document.multipart -> (gzip payload, content type)
pub fn multipart<'py>(py: Python<'py>, xml: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let raw = bytes_of(xml).ok_or_else(|| PyTypeError::new_err("can't concat str to bytes"))?;
    let (payload, ctype) = rs_doc::doc::multipart(&raw);
    Ok((PyBytes::new(py, &payload).into_any(), PyString::new(py, &ctype).into_any()))
}

#[cfg(feature = "python")]
/// document.new_document(title, body, local_id=..., now_ms=int)
pub fn new_document<'py>(py: Python<'py>, title: &Bound<'py, PyAny>, body: &Bound<'py, PyAny>, local_id: &Bound<'py, PyAny>, now_ms: i64) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    text_value(py, title)?;
    text_value(py, body)?;
    let local = if local_id.is_none() { None } else { Some(str_of(local_id)?) };
    let (identity, xml) = rs_doc::doc::new_document(&str_of(title)?, &str_of(body)?, local.as_deref(), rs_doc::doc::NowArg::Int(now_ms as i128), doc_limits(py)?.max_text)
        .map_err(|e| doc_err(py, e))?;
    Ok((PyString::new(py, &identity).into_any(), PyBytes::new(py, &xml).into_any()))
}

#[cfg(feature = "python")]
/// document.parse_document for a stored draft (title, body, local_id only).
pub fn parse_draft(py: Python<'_>, raw: &[u8]) -> PyResult<rs_doc::doc::DocumentSnapshot> {
    rs_doc::doc::parse_document(raw, "application/xml", None).map_err(|e| doc_err(py, e))
}

#[cfg(feature = "python")]
/// document.page_inventory(snapshot)
pub fn page_inventory<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let raw = bytes_of(&snapshot.getattr("resource_xml")?).ok_or_else(|| PyTypeError::new_err("a bytes-like object is required"))?;
    Ok(PyString::new(py, &rs_doc::doc::page_inventory(&raw).map_err(|e| doc_err(py, e))?).into_any())
}

#[cfg(feature = "python")]
pub fn namespace<'py>(py: Python<'py>, fields: &Bound<'py, PyDict>) -> PyResult<Bound<'py, PyAny>> {
    live(py, "types", "SimpleNamespace")?.call((), Some(fields))
}

#[cfg(feature = "python")]
/// document.prepare_edit(snapshot, title, body, now_ms=...) as an attribute view.
pub fn prepare_edit<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, title: &Bound<'py, PyAny>, body: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let reason = opt_str(&snapshot.getattr("read_only_reason")?);
    let s_title = opt_str(&snapshot.getattr("title")?);
    let s_body = opt_str(&snapshot.getattr("body")?);
    let resource = bytes_of(&snapshot.getattr("resource_xml")?).ok_or_else(|| PyTypeError::new_err("a bytes-like object is required"))?;
    let src = rs_doc::doc::EditSource {
        editable: snapshot.getattr("editable")?.is_truthy()?,
        read_only_reason: reason.as_deref(),
        title: s_title.as_deref(),
        body: s_body.as_deref(),
        resource_xml: &resource,
    };
    let invalid = || rs_doc::pyapi::document_error(py, "Metin geçersiz veya desteklenen boyutu aşıyor.");
    let (t, bd) = (opt_str(title).ok_or_else(invalid)?, opt_str(body).ok_or_else(invalid)?);
    let now = match rs_doc::pyapi::exact_int(now_ms) {
        Some(v) => rs_doc::doc::NowArg::Int(v),
        None if now_ms.is_none() => rs_doc::doc::NowArg::Missing,
        None => rs_doc::doc::NowArg::Invalid,
    };
    let e = rs_doc::doc::prepare_edit(&src, &t, &bd, now, &doc_limits(py)?).map_err(|e| doc_err(py, e))?;
    namespace(py, &dict(py, &[
        ("full_xml", b(py, &e.full_xml)),
        ("tree_xml", b(py, &e.tree_xml)),
        ("upload_xml", b(py, &e.upload_xml)),
        ("requested_ids", PyFrozenSet::new(py, &e.requested_ids)?.into_any()),
        ("title", s(py, &e.title)),
        ("body", s(py, &e.body)),
    ])?)
}

/// One MIME part with its body copied out.
pub struct OwnedPart {
    pub name: Option<String>,
    pub filename: Option<String>,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[cfg(feature = "python")]
fn snapshot_body(snapshot: &Bound<'_, PyAny>) -> PyResult<(Vec<u8>, String)> {
    let raw = bytes_of(&snapshot.getattr("raw_body")?).ok_or_else(|| PyTypeError::new_err("a bytes-like object is required"))?;
    Ok((raw, str_of(&snapshot.getattr("content_type")?)?))
}

#[cfg(feature = "python")]
/// `tools.notes_payload._parts(snapshot.raw_body, snapshot.content_type, max_parts=MAX_READONLY_PARTS)`
pub fn snapshot_parts(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<Vec<OwnedPart>> {
    let (raw, ctype) = snapshot_body(snapshot)?;
    let limits = rs_doc::pyapi::payload_limits(py)?;
    let parts = rs_doc::mime::parts(&raw, &ctype, limits.max_readonly_parts, &limits, &mut NoHooks).map_err(|e| doc_err(py, e))?;
    Ok(parts.into_iter().map(|p| OwnedPart { name: p.name, filename: p.filename, content_type: p.content_type, body: raw[p.body.clone()].to_vec() }).collect())
}

#[cfg(feature = "python")]
/// `[(p.name, p.filename, p.content_type, p.body) for p in parse_payload(raw, ctype).parts]`
pub fn payload_parts(py: Python<'_>, raw: &[u8], ctype: &Bound<'_, PyAny>) -> PyResult<Vec<(Option<String>, Option<String>, String, Vec<u8>)>> {
    let ctype = str_of(ctype)?;
    let limits = rs_doc::pyapi::payload_limits(py)?;
    let p = rs_doc::payload::parse_payload(raw, &ctype, None, &limits, &mut NoHooks).map_err(|e| doc_err(py, e))?;
    Ok(p.parts.iter().map(|part| (part.name.clone(), part.filename.clone(), part.content_type.clone(), raw[part.body.clone()].to_vec())).collect())
}

// ------------------------------------------------------------ editor

#[cfg(feature = "python")]
pub fn editor_content_fingerprint<'py>(py: Python<'py>, xml: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let raw = bytes_of(xml).ok_or_else(|| rs_doc::pyapi::document_error(py, "Belge parmak izi girdisi geçersiz."))?;
    Ok(PyString::new(py, &rs_editor::edit::editor_content_fingerprint(&raw).map_err(|e| editor_err(py, e))?).into_any())
}

#[cfg(feature = "python")]
pub fn title_content_fingerprint<'py>(py: Python<'py>, xml: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let raw = bytes_of(xml).ok_or_else(|| rs_doc::pyapi::document_error(py, "Belge parmak izi girdisi geçersiz."))?;
    Ok(PyString::new(py, &rs_editor::edit::title_content_fingerprint(&raw).map_err(|e| editor_err(py, e))?).into_any())
}

#[cfg(feature = "python")]
pub fn editor_fn<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    live(py, "samsung_notes.editor_document", name)
}

// ------------------------------------------------------------ ops cores

#[cfg(feature = "python")]
fn deps(py: Python<'_>) -> rs_ops::bridge::PyDeps<'_> {
    rs_ops::bridge::PyDeps { py }
}

#[cfg(feature = "python")]
/// page_style.prepare_page_background(snapshot, color_argb, now_ms=...)
pub fn prepare_page_background<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, color: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let e = rs_ops::page_style::prepare_page_background(&deps(py), &rs_ops::bridge::opaque(snapshot), rs_ops::ink_erase::exact_int(color), rs_ops::ink_erase::exact_int(now_ms))
        .map_err(|e| ops_err(py, e))?;
    namespace(py, &dict(py, &[
        ("full_xml", b(py, &e.full_xml)),
        ("tree_xml", b(py, &e.tree_xml)),
        ("upload_xml", b(py, &e.upload_xml)),
        ("page_ids", PyTuple::new(py, &e.page_ids)?.into_any()),
    ])?)
}

#[cfg(feature = "python")]
/// page_template.prepare_pdf_pages(...)
#[allow(clippy::too_many_arguments)]
pub fn prepare_pdf_pages<'py>(
    py: Python<'py>,
    snapshot: &Bound<'py, PyAny>,
    file_sha256: &str,
    upload_token: &Bound<'py, PyAny>,
    file_name: &Bound<'py, PyAny>,
    sizes: &Bound<'py, PyAny>,
    indices: &Bound<'py, PyAny>,
    template_type: &Bound<'py, PyAny>,
    ids: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    use rs_ops::ink_erase::exact_int;
    let mut page_sizes = Vec::new();
    for pair in sizes.try_iter()? {
        let pair = pair?;
        page_sizes.push((pair.get_item(0)?.extract::<f64>()?, pair.get_item(1)?.extract::<f64>()?));
    }
    let args = rs_ops::page_template::PdfArgs {
        file_sha256: Some(file_sha256.to_string()),
        upload_token: opt_str(upload_token),
        file_name: opt_str(file_name),
        page_sizes,
        now_ms: exact_int(now_ms),
        template_type: if template_type.is_none() { None } else { Some(exact_int(template_type)) },
        page_indices: Some(indices.try_iter()?.map(|v| Ok(exact_int(&v?))).collect::<PyResult<_>>()?),
        ids: if ids.is_none() { None } else { Some(ids.try_iter()?.map(|v| Ok(opt_str(&v?))).collect::<PyResult<_>>()?) },
    };
    let e = rs_ops::page_template::prepare_pdf_pages(&deps(py), &rs_ops::bridge::opaque(snapshot), &args).map_err(|e| ops_err(py, e))?;
    namespace(py, &dict(py, &[
        ("full_xml", b(py, &e.full_xml)),
        ("tree_xml", b(py, &e.tree_xml)),
        ("upload_xml", b(py, &e.upload_xml)),
        ("page_ids", PyTuple::new(py, &e.page_ids)?.into_any()),
        ("layer_ids", PyTuple::new(py, &e.layer_ids)?.into_any()),
    ])?)
}

#[cfg(feature = "python")]
fn page_ops_view<'py>(py: Python<'py>, e: rs_ops::page_ops::PageOps) -> PyResult<Bound<'py, PyAny>> {
    namespace(py, &dict(py, &[
        ("full_xml", b(py, &e.full_xml)),
        ("tree_xml", b(py, &e.tree_xml)),
        ("upload_xml", b(py, &e.upload_xml)),
        ("requested_ids", PyFrozenSet::new(py, &e.requested_ids)?.into_any()),
        ("page_ids", PyTuple::new(py, &e.page_ids)?.into_any()),
        ("layer_ids", PyTuple::new(py, &e.layer_ids)?.into_any()),
        ("content_file_ids", PyTuple::new(py, &e.content_file_ids)?.into_any()),
        ("page_order", PyTuple::new(py, &e.page_order)?.into_any()),
    ])?)
}

#[cfg(feature = "python")]
pub fn prepare_add_page<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, after: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>, page_id: &Bound<'py, PyAny>, layer_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    use rs_ops::ink_erase::exact_int;
    let (p, l) = (opt_str(page_id), opt_str(layer_id));
    let e = rs_ops::page_ops::prepare_add_page(&deps(py), &rs_ops::bridge::opaque(snapshot), exact_int(after), exact_int(now_ms), Some(p.as_deref()), Some(l.as_deref()), None, None)
        .map_err(|e| ops_err(py, e))?;
    page_ops_view(py, e)
}

#[cfg(feature = "python")]
pub fn prepare_delete_page<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, targets: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let ids = targets.try_iter()?.map(|v| Ok(opt_str(&v?))).collect::<PyResult<_>>()?;
    let e = rs_ops::page_ops::prepare_delete_page(&deps(py), &rs_ops::bridge::opaque(snapshot), ids, rs_ops::ink_erase::exact_int(now_ms)).map_err(|e| ops_err(py, e))?;
    page_ops_view(py, e)
}

#[cfg(feature = "python")]
pub fn prepare_reorder_pages<'py>(py: Python<'py>, snapshot: &Bound<'py, PyAny>, order: &Bound<'py, PyAny>, now_ms: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let ids = order.try_iter()?.map(|v| Ok(opt_str(&v?))).collect::<PyResult<_>>()?;
    let e = rs_ops::page_ops::prepare_reorder_pages(&deps(py), &rs_ops::bridge::opaque(snapshot), ids, rs_ops::ink_erase::exact_int(now_ms)).map_err(|e| ops_err(py, e))?;
    page_ops_view(py, e)
}

#[cfg(feature = "python")]
/// ink_erase.prepare_ink_delete / prepare_ink_move -> (attribute view, core edit).
pub fn prepare_stroke_edit<'py>(
    py: Python<'py>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    layer_id: &Bound<'py, PyAny>,
    ids: &Bound<'py, PyAny>,
    offset: Option<(&Bound<'py, PyAny>, &Bound<'py, PyAny>)>,
    now_ms: &Bound<'py, PyAny>,
) -> PyResult<(Bound<'py, PyAny>, rs_ops::ink_erase::StrokeEdit)> {
    use rs_ops::ink_erase::{exact_int, int_or_float, stroke_ids_arg};
    let (p, l) = (opt_str(page_id), opt_str(layer_id));
    let stroke_ids = stroke_ids_arg(ids)?;
    let snap = rs_ops::bridge::opaque(snapshot);
    let e = match offset {
        None => rs_ops::ink_erase::prepare_ink_delete(&deps(py), &snap, p.as_deref(), l.as_deref(), &stroke_ids, exact_int(now_ms)),
        Some((dx, dy)) => rs_ops::ink_erase::prepare_ink_move(&deps(py), &snap, p.as_deref(), l.as_deref(), &stroke_ids, int_or_float(dx), int_or_float(dy), exact_int(now_ms)),
    }
    .map_err(|e| ops_err(py, e))?;
    let parts = pyo3::types::PyList::empty(py);
    for part in &e.binary_parts {
        parts.append(namespace(py, &dict(py, &[
            ("name", s(py, &part.name)),
            ("filename", s(py, &part.filename)),
            ("content_type", s(py, "application/octet-stream")),
            ("body", b(py, &part.body)),
        ])?)?)?;
    }
    let view = namespace(py, &dict(py, &[
        ("kind", s(py, e.kind)),
        ("full_xml", b(py, &e.full_xml)),
        ("tree_xml", b(py, &e.tree_xml)),
        ("upload_xml", b(py, &e.upload_xml)),
        ("requested_ids", PyFrozenSet::new(py, &e.requested_ids)?.into_any()),
        ("page_id", s(py, &e.page_id)),
        ("layer_id", s(py, &e.layer_id)),
        ("stroke_ids", PyTuple::new(py, &e.stroke_ids)?.into_any()),
        ("modified_time", int(py, e.modified_time)),
        ("binary_parts", PyTuple::new(py, parts.iter())?.into_any()),
        ("content_file_ids", PyTuple::new(py, &e.content_file_ids)?.into_any()),
    ])?)?;
    Ok((view, e))
}

#[cfg(feature = "python")]
/// ink_erase.expected_stroke_parts(fresh, edit) as JSON-ready lists.
pub fn expected_stroke_parts<'py>(py: Python<'py>, fresh: &Bound<'py, PyAny>, e: &rs_ops::ink_erase::StrokeEdit) -> PyResult<Bound<'py, PyAny>> {
    let (raw, ctype) = snapshot_body(fresh)?;
    let payload = rs_ops::bridge::rust_payload(py, &raw, &ctype).map_err(|e| ops_err(py, e))?;
    let removed: Vec<Option<String>> = e.removed_part_filenames.iter().map(|n| Some(n.clone())).collect();
    let added: Vec<(Option<String>, Vec<u8>)> = e.binary_parts.iter().map(|p| (Some(p.filename.clone()), p.body.clone())).collect();
    let items = rs_ops::ink_erase::expected_stroke_parts(&payload.parts, &removed, &added).map_err(|e| ops_err(py, e))?;
    let list = pyo3::types::PyList::empty(py);
    for (f, h) in items {
        list.append(pyo3::types::PyList::new(py, [f.map(|x| s(py, &x)).unwrap_or_else(|| none(py)), s(py, &h)])?)?;
    }
    Ok(list.into_any())
}

#[cfg(feature = "python")]
/// ink_erase.check_compare_response(raw, baseline, edit)
pub fn check_compare_response(py: Python<'_>, raw: &Bound<'_, PyAny>, baseline: &Bound<'_, PyAny>, e: &rs_ops::ink_erase::StrokeEdit) -> PyResult<()> {
    let bytes = match raw.cast::<PyBytes>() {
        Ok(b) => b.as_bytes().to_vec(),
        Err(_) => return Err(ops_err(py, rs_ops::err::Error::new(rs_ops::err::Kind::Ink, "compare response bound"))),
    };
    let view = rs_ops::ink_erase::EditView {
        kind: e.kind.to_string(),
        page_id: Some(e.page_id.clone()),
        layer_id: Some(e.layer_id.clone()),
        stroke_ids: e.stroke_ids.iter().cloned().map(Some).collect(),
        content_file_ids: e.content_file_ids.iter().cloned().map(Some).collect(),
    };
    rs_ops::ink_erase::check_compare_response(&bytes, opt_str(baseline).as_deref(), &view).map_err(|err| ops_err(py, err))
}

#[cfg(feature = "python")]
/// ink_first_stroke.first_stroke_style(color, size) -> (colorARGB, penSize) Python values.
pub fn first_stroke_style<'py>(py: Python<'py>, color: &Bound<'py, PyAny>, size: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let style = rs_ops::ink_first_stroke::first_stroke_style(rs_ops::ink_erase::exact_int(color), rs_ops::ink_erase::int_or_float(size)).map_err(|e| ops_err(py, e))?;
    style_values(py, style)
}

#[cfg(feature = "python")]
/// ink_highlighter.highlighter_style(color, size) -> (colorARGB, penSize) Python values.
pub fn highlighter_style<'py>(py: Python<'py>, color: &Bound<'py, PyAny>, size: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let style = rs_ops::ink_highlighter::highlighter_style(rs_ops::ink_erase::exact_int(color), rs_ops::ink_erase::int_or_float(size)).map_err(|e| ops_err(py, e))?;
    style_values(py, style)
}

#[cfg(feature = "python")]
fn style_values<'py>(py: Python<'py>, style: rs_ops::ink_first_stroke::Style) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    use rs_ops::deps::Num;
    let get = |k: &str| -> PyResult<Bound<'py, PyAny>> {
        match style.iter().find(|(n, _)| n == k).map(|(_, v)| v) {
            Some(Num::I(i)) => Ok(int(py, *i)),
            Some(Num::F(f)) => Ok(pyo3::types::PyFloat::new(py, *f).into_any()),
            None => Err(pyo3::exceptions::PyKeyError::new_err(k.to_string())),
        }
    };
    Ok((get("colorARGB")?, get("penSize")?))
}

pub const FIRST_STROKE_RECIPE: i64 = rs_ops::ink_first_stroke::FIRST_STROKE_RECIPE;
pub const HIGHLIGHTER_RECIPE: i64 = rs_ops::ink_highlighter::HIGHLIGHTER_RECIPE;

#[cfg(feature = "python")]
/// ink_first_stroke.prepare_first_stroke(...) -> attribute view with one binary part.
#[allow(clippy::too_many_arguments)]
pub fn prepare_first_stroke<'py>(
    py: Python<'py>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    prepare_scratch_stroke(py, false, snapshot, page_id, samples, color, size, now_ms, new_stroke_id)
}

#[cfg(feature = "python")]
/// ink_highlighter.prepare_highlighter_stroke(...) -> attribute view with one binary part.
#[allow(clippy::too_many_arguments)]
pub fn prepare_highlighter_stroke<'py>(
    py: Python<'py>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    prepare_scratch_stroke(py, true, snapshot, page_id, samples, color, size, now_ms, new_stroke_id)
}

#[cfg(feature = "python")]
#[allow(clippy::too_many_arguments)]
fn prepare_scratch_stroke<'py>(
    py: Python<'py>,
    highlighter: bool,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    samples: &Bound<'py, PyAny>,
    color: &Bound<'py, PyAny>,
    size: &Bound<'py, PyAny>,
    now_ms: &Bound<'py, PyAny>,
    new_stroke_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    use rs_ops::ink_erase::{exact_int, int_or_float};
    let samples_obj = rs_ops::bridge::opaque(samples);
    let is_samples = samples.is_instance(&rs_ops::bridge::stroke_samples_class(py)?)?;
    let new_id = Some(opt_str(new_stroke_id));
    let prepare = if highlighter {
        rs_ops::ink_highlighter::prepare_highlighter_stroke
    } else {
        rs_ops::ink_first_stroke::prepare_first_stroke
    };
    let e = prepare(
        &deps(py),
        &rs_ops::bridge::opaque(snapshot),
        opt_str(page_id).as_deref(),
        if is_samples { Some(&samples_obj as &dyn std::any::Any) } else { None },
        exact_int(color),
        int_or_float(size),
        exact_int(now_ms),
        new_id.as_ref().map(|x| x.as_deref()),
    )
    .map_err(|e| ops_err(py, e))?;
    let part = namespace(py, &dict(py, &[
        ("name", s(py, &e.part.name)),
        ("filename", s(py, &e.part.filename)),
        ("content_type", s(py, "application/octet-stream")),
        ("body", b(py, &e.part.body)),
    ])?)?;
    namespace(py, &dict(py, &[
        ("full_xml", b(py, &e.full_xml)),
        ("tree_xml", b(py, &e.tree_xml)),
        ("upload_xml", b(py, &e.upload_xml)),
        ("requested_ids", PyFrozenSet::new(py, &e.requested_ids)?.into_any()),
        ("page_id", s(py, &e.page_id)),
        ("layer_id", s(py, &e.layer_id)),
        ("stroke_modified_time", int(py, e.tick)),
        ("binary_parts", PyTuple::new(py, [part])?.into_any()),
        ("content_file_ids", PyTuple::new(py, &e.content_file_ids)?.into_any()),
        ("recipe_version", int(py, if highlighter { HIGHLIGHTER_RECIPE } else { FIRST_STROKE_RECIPE })),
    ])?)
}
