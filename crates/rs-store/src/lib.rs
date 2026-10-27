//! Rust port of: storage, organization_store, editor_store, ink_store, preferences, shortcuts. Oracle: the Python test suite (see samsung_notes/_rust.py).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod casefold_table;
pub mod db;
pub mod host;
pub mod pyconv;
pub mod pystore;
pub mod val;
pub mod shapes;
pub mod storage;
pub mod organization_store;
pub mod editor_store;
pub mod ink_store;
pub mod preferences;
pub mod shortcuts;

#[cfg(feature = "python")]
#[pymodule]
fn _store(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "storage", storage::register)?;
    rs_common::submodule(m, "organization_store", organization_store::register)?;
    rs_common::submodule(m, "editor_store", editor_store::register)?;
    rs_common::submodule(m, "ink_store", ink_store::register)?;
    rs_common::submodule(m, "preferences", preferences::register)?;
    rs_common::submodule(m, "shortcuts", shortcuts::register)?;
    Ok(())
}
