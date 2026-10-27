//! Replacement for the Python module `preview_contract` (constants only).
//!
//! Since 2026-10-27 the canvas-cache decoder is pure Rust (notes-preview
//! `spi`); the emulator limits and the pinned native-library identity are
//! gone (historical copy: reference/spi-emulator-glue/).
use crate::pixels as px;
#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("MAX_INPUT", px::MAX_INPUT)?;
    m.add("MAX_PIXELS", px::MAX_PIXELS)?;
    m.add("MAX_OUTPUT", px::MAX_OUTPUT)?;
    rs_common::export(m, &["MAX_INPUT", "MAX_PIXELS", "MAX_OUTPUT"])
}
