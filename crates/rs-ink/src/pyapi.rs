//! PyO3 helpers safe to use from OTHER extension modules linking rs-ink as an rlib.
//! They never touch this cdylib's own type objects: classes are looked up live
//! from `samsung_notes.ink` / `samsung_notes.ink_editor` (Rust or Python).
use crate::core::prepare::InkAppendEdit;
use crate::core::InkErr;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyKeyError, PyOverflowError, PyTypeError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyBytes, PyFrozenSet, PyString, PyTuple};

#[cfg(feature = "python")]
fn live(py: Python<'_>, module: &str, name: &str, msg: String) -> PyErr {
    match py.import(module).and_then(|m| m.getattr(name)).and_then(|c| c.call1((msg,))) {
        Ok(inst) => PyErr::from_value(inst),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
/// Map a core error onto the live Python exception class the oracle raises.
pub fn ink_error(py: Python<'_>, e: InkErr) -> PyErr {
    match e {
        InkErr::Ink(m) => live(py, "samsung_notes.ink", "InkError", m),
        InkErr::Document(m) => rs_doc::pyapi::document_error(py, &m),
        InkErr::Payload(m) => rs_doc::pyapi::payload_error(py, &m),
        InkErr::Struct(m) => live(py, "struct", "error", m),
        InkErr::Parse(m) => live(py, "xml.etree.ElementTree", "ParseError", m),
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

#[cfg(feature = "python")]
/// Build the Python `samsung_notes.ink_editor.InkAppendEdit` (live class) for `edit`;
/// `title`/`body` are the snapshot's.
pub fn append_edit_to_py<'py>(py: Python<'py>, edit: &InkAppendEdit, title: Bound<'py, PyAny>, body: Bound<'py, PyAny>)
                              -> PyResult<Bound<'py, PyAny>> {
    let m = py.import("samsung_notes.ink_editor")?;
    let part_cls = m.getattr("InkBinaryPart")?;
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
        title,
        body,
        PyTuple::new(py, &edit.content_file_ids)?.into_any(),
        edit.recipe_version.into_pyobject(py)?.into_any(),
    ];
    m.getattr("InkAppendEdit")?.call1(PyTuple::new(py, args)?)
}
