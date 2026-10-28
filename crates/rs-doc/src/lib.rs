//! Rust port of: document, notes_payload, notes_writer. Oracle: the Python test suite (see samsung_notes/_rust.py).
//!
//! Pure-Rust API (no PyO3 types): [`xml`] (ElementTree-compatible tree),
//! [`mime`] (bounded multipart/header parsing), [`payload`] (`parse_payload`),
//! [`writer`] (`ResearchNote`, `object_hash`, `parent_hash`),
//! [`doc`] (`DocumentSnapshot`, `parse_document`, `prepare_edit`, ...).
//! The `document`, `notes_payload`, `notes_writer` modules hold the thin
//! Python bindings; `pyapi` has helpers other area crates may use.
// Without the `python` feature the PyO3 glue is compiled out; helpers and
// imports only that glue used are expected to be unused then.
#![cfg_attr(not(feature = "python"), allow(unused_imports, dead_code, unused_macros))]
#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod error;
pub mod util;
pub mod xml;
pub mod mime;
pub mod payload;
pub mod writer;
pub mod doc;
pub mod objects;
pub mod pyapi;

pub mod document;
pub mod notes_payload;
pub mod notes_writer;

pub use error::{Error, Result};

#[cfg(feature = "python")]
#[pymodule]
fn _doc(m: &Bound<'_, PyModule>) -> PyResult<()> {
    rs_common::submodule(m, "notes_payload", notes_payload::register)?;
    rs_common::submodule(m, "notes_writer", notes_writer::register)?;
    rs_common::submodule(m, "document", document::register)?;
    Ok(())
}
