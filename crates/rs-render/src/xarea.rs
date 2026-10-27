//! Cross-area access.
//!
//! * `class`: exception classes, dataclasses and constants are looked up in
//!   the module that owns them. Class identity lives in that namespace
//!   (other modules `except` on it), so this is not an implementation
//!   fallback and works whether the owner runs Python or Rust.
//! * Native: STABLE areas linked as rlibs (rs-doc, rs-editor, rs-ink, rs-ops, rs-store).
//! * `py_fallback`: remaining calls into another area's implementation
//!   (counted by tools/rust_conformance.py).
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use rs_common::py_fallback;

#[cfg(feature = "python")]
pub fn class<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<Bound<'py, PyAny>> {
    py.import(module)?.getattr(name)
}

#[cfg(feature = "python")]
pub fn raise(py: Python<'_>, module: &str, name: &str, message: &str) -> PyErr {
    match class(py, module, name).and_then(|c| c.call1((message,))) {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
pub fn store_error(py: Python<'_>, code: &str, message: &str, status: u16) -> PyErr {
    match class(py, "samsung_notes.storage", "StoreError").and_then(|c| c.call1((code, message, status))) {
        Ok(value) => PyErr::from_value(value),
        Err(error) => error,
    }
}

#[cfg(feature = "python")]
pub fn document_error(py: Python<'_>, message: &str) -> PyErr {
    raise(py, "samsung_notes.document", "DocumentError", message)
}

#[cfg(feature = "python")]
fn doc_err(py: Python<'_>, e: rs_doc::Error) -> PyErr {
    match e {
        rs_doc::Error::Document(m) => document_error(py, &m),
        rs_doc::Error::Value(m) => pyo3::exceptions::PyValueError::new_err(m.into_owned()),
        rs_doc::Error::Type(m) => pyo3::exceptions::PyTypeError::new_err(m.into_owned()),
        other => pyo3::exceptions::PyValueError::new_err(format!("{other:?}")),
    }
}

#[cfg(feature = "python")]
/// storage.private_directory (rs-store core); returns a pathlib.Path.
pub fn private_directory<'py>(py: Python<'py>, path: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let path = crate::pyhelp::path(py, path)?;
    let local: std::path::PathBuf = path.extract()?;
    match py.detach(|| rs_store::storage::private_directory(&local)) {
        Ok(_) => Ok(path),
        Err(rs_store::db::Error::Value(m)) => Err(pyo3::exceptions::PyValueError::new_err(m)),
        Err(e) => Err(pyo3::exceptions::PyOSError::new_err(format!("{e:?}"))),
    }
}

#[cfg(feature = "python")]
/// document.integer (rs-doc core).
pub fn integer(py: Python<'_>, value: Option<&str>) -> PyResult<i64> {
    rs_doc::doc::integer(value).map(|v| v as i64).map_err(|e| doc_err(py, e))
}

#[cfg(feature = "python")]
/// editor_document._decode / _b64 / utf16_length (rs-editor core).
pub fn editor_decode(py: Python<'_>, value: &str) -> PyResult<String> {
    rs_editor::rich::decode(value).map_err(|e| match e {
        rs_editor::rich::Error::Document(m) => document_error(py, &m),
        other => pyo3::exceptions::PyValueError::new_err(format!("{other:?}")),
    })
}

pub fn editor_b64(value: &str) -> String {
    rs_editor::rich::b64(value)
}

pub fn utf16_length(value: &str) -> usize {
    rs_editor::rich::utf16_length(value)
}

#[cfg(feature = "python")]
/// tools.notes_payload.parse_payload through the module: the oracle patches
/// `tools.notes_payload._parts` and counts one MIME parse per manifest, which
/// only the module-level function (rs-doc's, under NOTES_IMPL=rust) honours.
pub fn parse_payload<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    py_fallback(py, "tools.notes_payload", "parse_payload")
}

// ---- rs-ink core

use rs_ink::core::payload::{InkInventory, ParsedPayload};
use rs_ink::core::InkErr;

#[cfg(feature = "python")]
/// InkErr -> the Python exception class the oracle raises.
pub fn ink_err(py: Python<'_>, e: InkErr) -> PyErr {
    match e {
        InkErr::Ink(m) => raise(py, "samsung_notes.ink", "InkError", &m),
        InkErr::Document(m) => document_error(py, &m),
        InkErr::Payload(m) => raise(py, "tools.notes_payload", "PayloadError", &m),
        InkErr::Struct(m) => raise(py, "struct", "error", &m),
        InkErr::Parse(m) => raise(py, "xml.etree.ElementTree", "ParseError", &m),
        InkErr::KeyError(m) => pyo3::exceptions::PyKeyError::new_err(m),
        InkErr::ValueError(m) => pyo3::exceptions::PyValueError::new_err(m),
        InkErr::TypeError(m) => pyo3::exceptions::PyTypeError::new_err(m),
        InkErr::Overflow(m) => pyo3::exceptions::PyOverflowError::new_err(m),
        InkErr::Unicode(enc, data, start, end, reason) => {
            match py.get_type::<pyo3::exceptions::PyUnicodeDecodeError>()
                .call1((enc, pyo3::types::PyBytes::new(py, &data), start, end, reason)) {
                Ok(v) => PyErr::from_value(v),
                Err(e) => e,
            }
        }
        InkErr::Foreign => pyo3::exceptions::PyValueError::new_err("foreign error"),
    }
}

#[cfg(feature = "python")]
/// The PayloadResult fields ink.inspect_ink_payload reads.
fn parsed_payload(parsed: &Bound<'_, PyAny>) -> PyResult<ParsedPayload> {
    let opt = |v: Bound<'_, PyAny>| -> PyResult<Option<String>> { if v.is_none() { Ok(None) } else { Ok(Some(v.extract()?)) } };
    let mut out = ParsedPayload { locked: parsed.getattr("locked")?.is_truthy()?, xml_documents: vec![], content_files: vec![],
                                  download_urls: vec![], parts: vec![] };
    if out.locked {
        return Ok(out);
    }
    for x in parsed.getattr("xml_documents")?.try_iter()? {
        out.xml_documents.push(x?.extract()?);
    }
    for c in parsed.getattr("content_files")?.try_iter()? {
        let c = c?;
        out.content_files.push((opt(c.getattr("bind_number")?)?, c.getattr("file_hash")?.extract()?));
    }
    for d in parsed.getattr("download_urls")?.try_iter()? {
        let d = d?;
        out.download_urls.push((d.getattr("id")?.extract()?, d.getattr("url")?.extract()?));
    }
    for p in parsed.getattr("parts")?.try_iter()? {
        let p = p?;
        let body = p.getattr("body")?;
        let (len, head) = match body.cast::<pyo3::types::PyBytes>() {
            Ok(b) => (b.as_bytes().len(), b.as_bytes()[..b.as_bytes().len().min(12)].to_vec()),
            Err(_) => (0, vec![]),
        };
        out.parts.push((opt(p.getattr("name")?)?, len, head));
    }
    Ok(out)
}

#[cfg(feature = "python")]
/// ink.inspect_ink_payload on a Python PayloadResult (rs-ink core).
pub fn ink_inventory(py: Python<'_>, parsed: &Bound<'_, PyAny>) -> PyResult<InkInventory> {
    let input = parsed_payload(parsed)?;
    py.detach(|| rs_ink::core::payload::inspect_ink_payload(&input)).map_err(|e| ink_err(py, e))
}

#[cfg(feature = "python")]
/// The inventory's canvas candidates as samsung_notes.ink.CanvasPreviewCandidate objects.
pub fn candidates<'py>(py: Python<'py>, inventory: &InkInventory) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let class = class(py, "samsung_notes.ink", "CanvasPreviewCandidate")?;
    inventory.canvas_candidates.iter().map(|c| class.call1((c.page_index, c.key, c.width, c.height, c.is_dark_mode,
        &c.bind_number, &c.sha256, c.download_url.as_deref()))).collect()
}

#[cfg(feature = "python")]
pub fn inventory_object<'py>(py: Python<'py>, inventory: &InkInventory) -> PyResult<Bound<'py, PyAny>> {
    class(py, "samsung_notes.ink", "InkInventory")?.call1((inventory.stroke_parts, inventory.wrapped_v4000_parts,
        inventory.page_count, pyo3::types::PyTuple::new(py, candidates(py, inventory)?)?, inventory.unresolved_canvas_references))
}

#[cfg(feature = "python")]
/// ink.inspect_spi (rs-ink core).
pub fn inspect_spi(py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<rs_ink::core::binary::SpiHeader> {
    let Ok(bytes) = data.cast::<pyo3::types::PyBytes>() else {
        return Err(raise(py, "samsung_notes.ink", "InkError", "SPI input exceeds bounds or is truncated"));
    };
    rs_ink::core::binary::inspect_spi(bytes.as_bytes()).map_err(|e| ink_err(py, e))
}

// ---- rs-ops / rs-editor

/// ink_projection.verified_resource result: a disabled reason, or the
/// verified payload's (name, body) parts.
pub enum Verified {
    Reason(&'static str),
    Parts(Vec<(Option<String>, Vec<u8>)>),
}

#[cfg(feature = "python")]
/// ink_projection.verified_resource (rs-ops core) on a DocumentSnapshot.
pub fn verified_resource(py: Python<'_>, snapshot: &Bound<'_, PyAny>) -> PyResult<Verified> {
    let snap = rs_ops::deps::Snapshot {
        raw_body: snapshot.getattr("raw_body")?.extract()?,
        content_type: snapshot.getattr("content_type")?.extract()?,
        resource_xml: snapshot.getattr("resource_xml")?.extract()?,
        local_id: snapshot.getattr("local_id")?.extract()?,
        commit_id: snapshot.getattr("commit_id")?.extract()?,
        handle: None,
    };
    let deps = rs_ops::bridge::PyDeps { py };
    match rs_ops::ink_projection::verify(&deps, &snap) {
        Err(e) => Err(rs_ops::bridge::to_py(py, e)),
        Ok(rs_ops::ink_projection::Verified::Reason(r)) => Ok(Verified::Reason(r)),
        Ok(rs_ops::ink_projection::Verified::Ok(payload, _)) => {
            Ok(Verified::Parts(payload.parts.into_iter().map(|p| (p.name, p.body)).collect()))
        }
    }
}

#[cfg(feature = "python")]
fn editor_err(py: Python<'_>, e: rs_editor::rich::Error) -> PyErr {
    use rs_editor::rich::Error as E;
    match e {
        E::Document(m) => document_error(py, &m),
        E::Value(m) => pyo3::exceptions::PyValueError::new_err(m),
        E::Type(m) => pyo3::exceptions::PyTypeError::new_err(m),
        E::Overflow(m) => pyo3::exceptions::PyOverflowError::new_err(m),
        E::Attribute(m) => pyo3::exceptions::PyAttributeError::new_err(m),
        E::Parse(m) => raise(py, "xml.etree.ElementTree", "ParseError", &m),
        E::Payload(m) => raise(py, "tools.notes_payload", "PayloadError", &m),
        E::External { message, .. } => pyo3::exceptions::PyValueError::new_err(message),
    }
}

#[cfg(feature = "python")]
fn val_to_py<'py>(py: Python<'py>, v: &rs_editor::pyval::Val) -> PyResult<Bound<'py, PyAny>> {
    use rs_editor::pyval::Val;
    Ok(match v {
        Val::None => py.None().into_bound(py),
        Val::Bool(b) => pyo3::types::PyBool::new(py, *b).to_owned().into_any(),
        Val::Int(i, _) => py.import("builtins")?.getattr("int")?.call1((&i.0,))?,
        Val::Float(f) => f.into_pyobject(py)?.into_any(),
        Val::Str(s) => pyo3::types::PyString::new(py, s).into_any(),
        Val::BadStr(_) | Val::Other(_) => return Err(pyo3::exceptions::PyValueError::new_err("unrepresentable rich-text value")),
        Val::List(items) => pyo3::types::PyList::new(py, items.iter().map(|i| val_to_py(py, i)).collect::<PyResult<Vec<_>>>()?)?.into_any(),
        Val::Dict(items) => {
            let d = pyo3::types::PyDict::new(py);
            for (k, value) in items {
                d.set_item(val_to_py(py, k)?, val_to_py(py, value)?)?;
            }
            d.into_any()
        }
    })
}

#[cfg(feature = "python")]
/// editor_document._object_rich(obj).model for an `object` element whose
/// `text` child holds `encoded`, plus the given side-car list elements.
pub fn object_rich_model<'py>(py: Python<'py>, encoded: String, sidecars: &[Bound<'py, PyAny>]) -> PyResult<Bound<'py, PyAny>> {
    use rs_editor::etree::Element;
    #[cfg(feature = "python")]
    fn mirror(e: &Bound<'_, PyAny>) -> PyResult<Element> {
        let tag = e.getattr("tag")?;
        let Ok(tag) = tag.extract::<String>() else {
            return Err(pyo3::exceptions::PyValueError::new_err("unsupported rich-text metadata element"));
        };
        let mut out = Element::new(tag);
        for (k, v) in e.getattr("attrib")?.cast::<pyo3::types::PyDict>()?.iter() {
            out.attrib.push((k.extract()?, v.extract()?));
        }
        out.text = e.getattr("text")?.extract()?;
        out.tail = e.getattr("tail")?.extract()?;
        for child in e.try_iter()? {
            out.children.push(mirror(&child?)?);
        }
        Ok(out)
    }
    let mut clean = Element::new("object");
    let mut text = Element::new("text");
    text.text = Some(encoded);
    clean.children.push(text);
    for element in sidecars {
        clean.children.push(mirror(element)?);
    }
    let rich = py.detach(|| rs_editor::edit::object_rich(&clean)).map_err(|e| editor_err(py, e))?;
    val_to_py(py, &rich.model)
}

