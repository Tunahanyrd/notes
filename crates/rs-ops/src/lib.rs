//! Rust port of: ink_projection, ink_erase, ink_first_stroke, page_style, page_template, page_ops. Oracle: the Python test suite (see samsung_notes/_rust.py).
//!
//! Core logic is plain Rust over `xml::Element` and the `deps::Deps`
//! interface; `bridge` holds the PyO3 boundary and the Python-backed `Deps`.
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod bridge;
pub mod deps;
pub mod err;
pub mod record;
pub mod util;
pub mod xml;

pub mod ink_projection;
pub mod ink_erase;
pub mod ink_first_stroke;
pub mod ink_highlighter;
pub mod page_style;
pub mod page_template;
pub mod page_ops;
pub mod page_shapes;

#[cfg(feature = "python")]
#[pymodule]
fn _ops(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "ink_projection", ink_projection::register)?;
    rs_common::submodule(m, "ink_erase", ink_erase::register)?;
    rs_common::submodule(m, "ink_first_stroke", ink_first_stroke::register)?;
    rs_common::submodule(m, "ink_highlighter", ink_highlighter::register)?;
    rs_common::submodule(m, "page_style", page_style::register)?;
    rs_common::submodule(m, "page_template", page_template::register)?;
    rs_common::submodule(m, "page_ops", page_ops::register)?;
    rs_common::submodule(m, "page_shapes", page_shapes::register)?;
    Ok(())
}
