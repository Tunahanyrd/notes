//! Rust port of: ink, ink_coordinates, ink_cache, ink_editor, ink_capture. Oracle: the Python test suite (see samsung_notes/_rust.py).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod core;
pub mod pyapi;
mod pyglue;

pub mod ink;
pub mod ink_coordinates;
pub mod ink_cache;
pub mod ink_editor;
pub mod ink_capture;

#[cfg(feature = "python")]
#[pymodule]
fn _ink(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "ink", ink::register)?;
    rs_common::submodule(m, "ink_coordinates", ink_coordinates::register)?;
    rs_common::submodule(m, "ink_cache", ink_cache::register)?;
    rs_common::submodule(m, "ink_editor", ink_editor::register)?;
    rs_common::submodule(m, "ink_capture", ink_capture::register)?;
    Ok(())
}
