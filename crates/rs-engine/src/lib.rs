//! Rust port of: engine, server, ui_assets (+ the `python -m samsung_notes` launcher).
//! Oracle: the Python test suite (see samsung_notes/_rust.py).
//!
//! Pure-Rust layers (no PyO3 types): `pyjson` (CPython-compatible JSON),
//! `pyhttp` (BaseHTTPRequestHandler wire behaviour), `urlparse`, `mime`,
//! `server::core` (routing, validation, static files) and `ui_assets::local_font`.
//! The PyO3 classes are thin adapters that keep the Python-visible API.
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod pytables;
pub mod pyjson;
pub mod urlparse;
pub mod mime;
pub mod pyhttp;
pub mod pyconv;
pub mod engine_core;
pub mod engine;
pub mod server_core;
pub mod server;
pub mod ui_assets;
pub mod launcher;

#[cfg(feature = "python")]
#[pymodule]
fn _engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "engine", engine::register)?;
    rs_common::submodule(m, "server", server::register)?;
    rs_common::submodule(m, "ui_assets", ui_assets::register)?;
    rs_common::submodule(m, "__main__", launcher::register)?;
    Ok(())
}
