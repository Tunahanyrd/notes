//! Rust port of: storage, organization_store, editor_store, ink_store, preferences, shortcuts. Oracle: the Python test suite (see samsung_notes/_rust.py).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(
    not(feature = "python"),
    allow(unused_imports, dead_code, unused_macros)
)]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod casefold_table;
pub mod db;
pub mod document_store;
pub mod media_store;
pub mod note_container;
pub mod note_container_page;
pub mod page_context;
#[cfg(feature = "read-tracing")]
pub mod read_trace;
pub mod preview_inventory;
pub mod preview_render_index;
pub mod editor_store;
pub mod host;
pub mod ink_store;
pub mod organization_store;
pub mod preferences;
pub mod pyconv;
pub mod pystore;
pub mod shapes;
pub mod shortcuts;
pub mod snapshot_store;
pub mod snapshot_gc;
pub mod snapshot_migration;
pub mod storage;
pub mod val;

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
