//! Replacement for the Python module `page_image` (numpy no longer needed).
use crate::pixels as px;
#[cfg(feature = "python")]
use crate::pdf_background::rgba_or_raise;
#[cfg(feature = "python")]
use crate::preview_worker::encode;
#[cfg(feature = "python")]
use pyo3::exceptions::{PyValueError, PyZeroDivisionError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::pybacked::PyBackedBytes;
#[cfg(feature = "python")]
use pyo3::types::PyBytes;

#[cfg(feature = "python")]
#[pyfunction]
pub fn light_from_dark<'py>(py: Python<'py>, png: PyBackedBytes, width: i64, height: i64) -> PyResult<Bound<'py, PyBytes>> {
    let mut pixels = rgba_or_raise(py, &png, width, height)?;
    py.detach(|| px::light_from_dark_pixels(&mut pixels));
    encode(py, width, height, &pixels)
}

#[cfg(feature = "python")]
pub fn downsample(py: Python<'_>, source: &[u8], width: usize, height: usize, target_w: usize, target_h: usize,
                  background: i64) -> PyResult<Vec<u8>> {
    py.detach(|| px::box_downsample(source, width, height, target_w, target_h, background)).map_err(|e| match e {
        px::DownsampleError::Length => PyValueError::new_err("cannot reshape array: pixel buffer size mismatch"),
        px::DownsampleError::ZeroDivision => PyZeroDivisionError::new_err("integer division or modulo by zero"),
        px::DownsampleError::ByteRange => PyValueError::new_err("bytes must be in range(0, 256)"),
    })
}

#[cfg(feature = "python")]
#[pyfunction]
pub fn box_downsample<'py>(py: Python<'py>, source: PyBackedBytes, width: usize, height: usize, target_w: usize,
                           target_h: usize, background: i64) -> PyResult<Bound<'py, PyBytes>> {
    let out = downsample(py, &source, width, height, target_w, target_h, background)?;
    Ok(PyBytes::new(py, &out))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(light_from_dark, m)?)?;
    m.add_function(wrap_pyfunction!(box_downsample, m)?)?;
    rs_common::export(m, &["light_from_dark", "box_downsample"])
}
