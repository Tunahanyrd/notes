//! Replacement for the Python module `cloud_shapes`: one shape/line object
//! added to a page as an immutable Cloud transaction (offline builder:
//! `samsung_notes.page_shapes`, called through its live module).
#[cfg(feature = "python")]
use crate::cloud_api::check_uuid;
#[cfg(feature = "python")]
use crate::cloud_page_ops::{begin, compare, expected, page_order, plan, reconcile_kinds};
use crate::glue::*;
#[cfg(feature = "python")]
use pyo3::exceptions::PyTypeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyFrozenSet, PyList, PyString};

pub const ADD_SHAPE_KIND: &str = "add_shape_v1";
pub const KINDS: [&str; 1] = [ADD_SHAPE_KIND];
const ALLOWED: [&str; 5] = ["kind", "geometry", "colorARGB", "width", "fillARGB"];
const STYLE: [&str; 3] = ["colorARGB", "width", "fillARGB"];

#[cfg(feature = "python")]
fn page_shapes<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    crate::areas::live(py, "samsung_notes.page_shapes", name)
}

#[cfg(feature = "python")]
fn shape_error(py: Python<'_>, message: &str) -> PyErr {
    match page_shapes(py, "ShapeError").and_then(|c| c.call1((message,))) {
        Ok(v) => PyErr::from_value(v),
        Err(e) => e,
    }
}

#[cfg(feature = "python")]
fn is_shape_error(py: Python<'_>, e: &PyErr) -> bool {
    page_shapes(py, "ShapeError").map(|c| e.value(py).is_instance(&c).unwrap_or(false)).unwrap_or(false)
}

#[cfg(feature = "python")]
fn shape_intent_impl<'py>(py: Python<'py>, shape: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let normalized = (|| -> PyResult<Bound<'py, PyAny>> {
        let d = shape.cast::<PyDict>().map_err(|_| shape_error(py, "Şekil biçimi geçersiz."))?;
        for key in d.keys().iter() {
            let ok = key.cast::<PyString>().ok().and_then(|k| k.to_str().ok().map(|k| ALLOWED.contains(&k))).unwrap_or(false);
            if !ok {
                return Err(shape_error(py, "Şekil biçimi geçersiz."));
            }
        }
        let style = PyDict::new(py);
        for k in STYLE {
            if let Some(v) = d.get_item(k)? {
                style.set_item(k, v)?;
            }
        }
        let kind = d.get_item("kind")?.unwrap_or_else(|| none(py));
        let geometry = d.get_item("geometry")?.unwrap_or_else(|| none(py));
        page_shapes(py, "normalize_shape")?.call1((kind, geometry, style))
    })();
    let normalized = match normalized {
        Ok(v) => v,
        Err(e) if is_shape_error(py, &e) => return Err(unsupported_from(py, &e)),
        Err(e) => return Err(e),
    };
    let (kind, geometry, style) = (normalized.get_item(0)?, normalized.get_item(1)?, normalized.get_item(2)?);
    let geometry = PyList::new(py, geometry.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
    dict(py, &[
        ("kind", kind),
        ("geometry", geometry.into_any()),
        ("colorARGB", style.get_item("colorARGB")?),
        ("width", style.get_item("width")?),
        ("fillARGB", style.get_item("fillARGB")?),
    ])
}

#[cfg(feature = "python")]
#[pyfunction]
fn shape_intent<'py>(shape: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    shape_intent_impl(shape.py(), shape)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (api, server_id, snapshot, page_id, shape, *, operation_id, now_ms = None))]
#[allow(clippy::too_many_arguments)]
fn add_shape<'py>(
    api: &Bound<'py, PyAny>,
    server_id: &Bound<'py, PyAny>,
    snapshot: &Bound<'py, PyAny>,
    page_id: &Bound<'py, PyAny>,
    shape: &Bound<'py, PyAny>,
    operation_id: &Bound<'py, PyAny>,
    now_ms: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = api.py();
    let now_ms = now_ms.cloned().unwrap_or_else(|| none(py));
    check_uuid(py, page_id)?;
    let spec = shape_intent_impl(py, shape)?;
    let snapshot = crate::cloud_api::coerce_snapshot(py, snapshot, server_id)?;
    let intent = dict(py, &[
        ("kind", s(py, ADD_SHAPE_KIND)),
        ("page_id", page_id.clone()),
        ("recipeVersion", page_shapes(py, "SHAPE_RECIPE")?),
        ("shape", spec.clone().into_any()),
    ])?;
    let begun = match begin(api, server_id, &snapshot, &now_ms, operation_id, &intent)? {
        Ok(b) => b,
        Err(replay) => return Ok(replay),
    };
    let plan = plan(api, &begun.output, &now_ms, 1)?;
    let object_id = plan.get_item("ids")?.get_item(0)?;
    let prepared = (|| -> PyResult<Bound<'py, PyAny>> {
        let style = PyDict::new(py);
        for k in STYLE {
            style.set_item(k, spec.get_item(k)?.ok_or_else(|| PyTypeError::new_err(k))?)?;
        }
        let kw = dict(py, &[("new_object_id", object_id.clone())])?;
        let edit = page_shapes(py, "prepare_add_shape")?.call(
            (&begun.fresh, page_id, spec.get_item("kind")?, spec.get_item("geometry")?, style, plan.get_item("now_ms")?),
            Some(&kw),
        )?;
        let mut req = vec![page_id.clone(), edit.getattr("layer_id")?, object_id.clone()];
        req.extend(edit.getattr("content_file_ids")?.try_iter()?.collect::<PyResult<Vec<_>>>()?);
        if edit.getattr("binary_parts")?.is_truthy()?
            || edit.getattr("object_id")?.ne(&object_id)?
            || edit.getattr("page_id")?.ne(page_id)?
            || edit.getattr("requested_ids")?.ne(PyFrozenSet::new(py, &req)?)?
        {
            return Err(shape_error(py, "Beklenmeyen şekil değişikliği."));
        }
        Ok(edit)
    })();
    let edit = match prepared {
        Ok(e) => e,
        Err(e) if caught(py, &e, &[K::Doc]) => return Err(unsupported_from(py, &e)),
        Err(e) => return Err(e),
    };
    let full = edit.getattr("full_xml")?;
    let order = page_order(py, &full)?;
    let exp = expected(py, &snapshot, server_id, &full, begun.opaque.clone(), order)?;
    let out = fspath(&begun.output)?;
    json_once(py, &out.join("expected.json"), exp.as_any())?;
    save_once(py, &out.join("draft.xml"), &bytes_of(&full).ok_or_else(|| PyTypeError::new_err("bytes expected"))?)?;
    let commit = snapshot.getattr("commit_id")?;
    let guard = |raw: &Bound<'py, PyAny>| {
        api.call_method(
            "_compare_ink_guard",
            (raw, &commit, page_id, edit.getattr("layer_id")?, &object_id),
            Some(&dict(py, &[("content_file_ids", edit.getattr("content_file_ids")?)])?),
        )
    };
    compare(api, server_id, &begun.output, &edit, guard, "Sunucunun istediği öğeler şekil eklemesiyle uyuşmuyor.")?;
    let (payload, ctype) = crate::areas::multipart(py, &edit.getattr("upload_xml")?)?;
    let url = str_of(&api.call_method1("_url", (server_id,))?)? + "/update";
    api.call_method1("_publish", (&begun.output, url, payload, ctype))
}

#[cfg(feature = "python")]
#[pyfunction]
fn reconcile<'py>(api: &Bound<'py, PyAny>, operation_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    reconcile_kinds(api, operation_id, &KINDS)
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(shape_intent, m)?)?;
    m.add_function(wrap_pyfunction!(add_shape, m)?)?;
    m.add_function(wrap_pyfunction!(reconcile, m)?)?;
    rs_common::export(m, &["shape_intent", "add_shape", "reconcile"])
}
