//! Replacement for the Python module `ink`.
use crate::core::{binary, payload, InkErr};
#[cfg(feature = "python")]
use crate::pyglue::{bytes_of, cls, err, ink_error, IntoPy};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyDict, PyTuple};

#[cfg(feature = "python")]
fn ink_err(py: Python<'_>, msg: &str) -> PyErr {
    err(py, InkErr::Ink(msg.into()))
}

#[cfg(feature = "python")]
#[pyfunction]
fn unwrap_stroke_resource<'py>(py: Python<'py>, data: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let Ok(b) = data.cast::<PyBytes>() else {
        return Err(ink_err(py, "stroke resource exceeds bounds or is empty"));
    };
    Ok(PyBytes::new(py, binary::unwrap_stroke_resource(b.as_bytes()).py(py)?))
}

#[cfg(feature = "python")]
#[pyfunction]
fn inspect_spi<'py>(py: Python<'py>, data: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let Ok(b) = data.cast::<PyBytes>() else {
        return Err(ink_err(py, "SPI input exceeds bounds or is truncated"));
    };
    let h = binary::inspect_spi(b.as_bytes()).py(py)?;
    cls(py, "SpiHeader")?.call1((h.width, h.height, h.color_index, h.row_group_size, h.reference_cache_capacity, h.flags,
                                  h.encoded_data_bytes))
}

#[cfg(feature = "python")]
fn opt_string(v: Bound<'_, PyAny>) -> PyResult<Option<String>> {
    if v.is_none() { Ok(None) } else { Ok(Some(v.extract()?)) }
}

#[cfg(feature = "python")]
/// Read the PayloadResult fields used by inspect_ink_payload.
fn parsed_from(parsed: &Bound<'_, PyAny>) -> PyResult<payload::ParsedPayload> {
    let mut out = payload::ParsedPayload {
        locked: parsed.getattr("locked")?.is_truthy()?,
        xml_documents: vec![],
        content_files: vec![],
        download_urls: vec![],
        parts: vec![],
    };
    if out.locked {
        return Ok(out);
    }
    for x in parsed.getattr("xml_documents")?.try_iter()? {
        out.xml_documents.push(bytes_of(&x?)?);
    }
    for c in parsed.getattr("content_files")?.try_iter()? {
        let c = c?;
        out.content_files.push((opt_string(c.getattr("bind_number")?)?, c.getattr("file_hash")?.extract()?));
    }
    for d in parsed.getattr("download_urls")?.try_iter()? {
        let d = d?;
        out.download_urls.push((d.getattr("id")?.extract()?, d.getattr("url")?.extract()?));
    }
    for p in parsed.getattr("parts")?.try_iter()? {
        let p = p?;
        let name = opt_string(p.getattr("name")?)?;
        let body = p.getattr("body")?;
        let (len, head) = match body.cast::<PyBytes>() {
            Ok(b) => (b.as_bytes().len(), b.as_bytes()[..b.as_bytes().len().min(12)].to_vec()),
            Err(_) => (0, vec![]),
        };
        out.parts.push((name, len, head));
    }
    Ok(out)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (raw, content_type=None))]
fn inspect_ink_payload<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>, content_type: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let payload_mod = py.import("tools.notes_payload")?;
    let inv = if raw.is_instance(&payload_mod.getattr("PayloadResult")?)? {
        payload::inspect_ink_payload(&parsed_from(raw)?).py(py)?
    } else {
        let ct = content_type.and_then(|c| c.cast::<pyo3::types::PyString>().ok());
        match (raw.cast::<PyBytes>(), ct) {
            (Ok(b), Some(ct)) => {
                let limits = rs_doc::pyapi::payload_limits(py)?;
                payload::inspect_raw(b.as_bytes(), ct.to_str()?, &limits).py(py)?
            }
            _ => {
                // Unusual argument types: the Python-facing parser raises the oracle's error.
                let kw = PyDict::new(py);
                kw.set_item("max_parts", payload_mod.getattr("MAX_READONLY_PARTS")?)?;
                let none = py.None().into_bound(py);
                let parsed = match payload_mod.getattr("parse_payload")?.call((raw, content_type.unwrap_or(&none)), Some(&kw)) {
                    Ok(p) => p,
                    Err(e) if e.is_instance(py, &payload_mod.getattr("PayloadError")?) => {
                        return Err(err(py, InkErr::Ink(e.value(py).str()?.to_cow()?.into_owned())));
                    }
                    Err(e) => return Err(e),
                };
                payload::inspect_ink_payload(&parsed_from(&parsed)?).py(py)?
            }
        }
    };
    let cand_cls = cls(py, "CanvasPreviewCandidate")?;
    let mut cands = Vec::new();
    for c in &inv.canvas_candidates {
        cands.push(cand_cls.call1((c.page_index, c.key, c.width, c.height, c.is_dark_mode, &c.bind_number, &c.sha256,
                                   c.download_url.as_deref()))?);
    }
    cls(py, "InkInventory")?.call1((inv.stroke_parts, inv.wrapped_v4000_parts, inv.page_count, PyTuple::new(py, cands)?,
                                    inv.unresolved_canvas_references))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("InkError", ink_error(m.py()))?;
    crate::pyglue::add_classes(m, &["CanvasPreviewCandidate", "InkInventory", "SpiHeader"])?;
    m.add_function(wrap_pyfunction!(unwrap_stroke_resource, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_spi, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_ink_payload, m)?)?;
    rs_common::export(m, &["InkError", "CanvasPreviewCandidate", "InkInventory", "SpiHeader", "unwrap_stroke_resource",
                           "inspect_ink_payload", "inspect_spi"])
}
