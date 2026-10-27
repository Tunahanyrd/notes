//! Shared helpers for the Rust port. The Python test suite is the oracle:
//! every `rs-<area>` crate builds a Python extension `_<area>` whose submodules
//! (named like the Python modules) export replacements listed in `__all__`.
//! Everything here is PyO3 glue: compiled only with the `python` feature.
#![cfg(feature = "python")]
use pyo3::prelude::*;
use pyo3::types::{PyList, PyModule};

/// Declare which names this submodule replaces (read by samsung_notes/_rust.py).
pub fn export(m: &Bound<'_, PyModule>, names: &[&str]) -> PyResult<()> {
    m.setattr("__all__", PyList::new(m.py(), names)?)
}

/// Add a submodule named like the Python module it replaces.
pub fn submodule<'py>(
    parent: &Bound<'py, PyModule>,
    name: &str,
    register: fn(&Bound<'py, PyModule>) -> PyResult<()>,
) -> PyResult<()> {
    let sub = PyModule::new(parent.py(), name)?;
    register(&sub)?;
    if !sub.hasattr("__all__")? {
        export(&sub, &[])?;
    }
    parent.add_submodule(&sub)
}

/// Temporary bridge to a Python implementation that is not ported yet.
/// Every use is counted by tools/rust_conformance.py (search "PYFALLBACK").
pub fn py_fallback<'py>(py: Python<'py>, module: &str, name: &str) -> PyResult<Bound<'py, PyAny>> {
    // PYFALLBACK
    py.import(module)?.getattr(name)
}
