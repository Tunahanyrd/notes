//! Rust port of: editor_document. Oracle: the Python test suite (see samsung_notes/_rust.py).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod editor_document;
pub mod edit;
pub mod etree;
pub mod pyval;
pub mod rich;

#[cfg(feature = "python")]
#[pymodule]
fn _editor(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "editor_document", editor_document::register)?;
    Ok(())
}
