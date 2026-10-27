//! Replacement for the Python module `ink_cache`.
use crate::core::cache;
use crate::core::num::Val;
#[cfg(feature = "python")]
use crate::pyglue::{err, replay, tree_from, val};
#[cfg(feature = "python")]
use pyo3::exceptions::PyTypeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::PyTuple;

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (root, page, *, tick))]
fn release_page_cache<'py>(py: Python<'py>, root: &Bound<'py, PyAny>, page: &Bound<'py, PyAny>, tick: &Bound<'py, PyAny>)
                           -> PyResult<Bound<'py, PyTuple>> {
    let tick = match val(tick) {
        Val::Int(i) => i,
        Val::Big(neg) => if neg { i128::MIN } else { i128::MAX },
        _ => return Err(PyTypeError::new_err("integer tick required")),
    };
    let (mut tree, objs) = tree_from(root, false)?;
    let page_id = objs.iter().position(|o| o.is(page));
    tree.log = Some(Vec::new());
    let result = cache::release_page_cache(&mut tree, page_id, tick);
    replay(&objs, tree.log.as_deref().unwrap_or(&[]))?;
    match result {
        Ok(changed) => PyTuple::new(py, changed),
        Err(e) => Err(err(py, e)),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn pdf_background(py: Python<'_>, page: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let (tree, _) = tree_from(page, false)?;
    cache::pdf_background(&tree, tree.root).map_err(|e| err(py, e))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(release_page_cache, m)?)?;
    m.add_function(wrap_pyfunction!(pdf_background, m)?)?;
    rs_common::export(m, &["release_page_cache", "pdf_background"])
}
