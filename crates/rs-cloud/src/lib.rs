//! Rust port of: cloud_api, cloud_metadata, cloud_ink_erase, cloud_page_ops, cloud_shapes. Oracle: the Python test suite (see samsung_notes/_rust.py).
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod etree;
pub mod guards;
pub mod pyjson;
pub mod util;
pub mod glue;
pub mod areas;

pub mod cloud_api;
pub mod cloud_metadata;
pub mod cloud_ink_erase;
pub mod cloud_page_ops;
pub mod cloud_shapes;

#[cfg(feature = "python")]
#[pymodule]
fn _cloud(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "cloud_api", cloud_api::register)?;
    rs_common::submodule(m, "cloud_metadata", cloud_metadata::register)?;
    rs_common::submodule(m, "cloud_ink_erase", cloud_ink_erase::register)?;
    rs_common::submodule(m, "cloud_page_ops", cloud_page_ops::register)?;
    rs_common::submodule(m, "cloud_shapes", cloud_shapes::register)?;
    Ok(())
}
