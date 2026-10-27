//! Replacement for the Python module `preview_worker`: only the PNG encoder
//! remains. The emulator worker entry point (which ran the native SPI codec
//! under Unicorn) is gone; historical copy in reference/spi-emulator-glue/.
use crate::pixels as px;
#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::pybacked::PyBackedBytes;
#[cfg(feature = "python")]
use pyo3::types::PyBytes;

#[cfg(feature = "python")]
/// RGBA8 straight-alpha PNG (filter 0, zlib level 6), bytes-identical to Python.
#[pyfunction]
pub fn rgba_png<'py>(py: Python<'py>, width: i64, height: i64, pixels: PyBackedBytes) -> PyResult<Bound<'py, PyBytes>> {
    let data: &[u8] = &pixels;
    let png = py.detach(|| px::rgba_png(width, height, data)).map_err(PyValueError::new_err)?;
    Ok(PyBytes::new(py, &png))
}

#[cfg(feature = "python")]
/// Shared by the Rust wrappers: encode or raise ValueError.
pub fn encode<'py>(py: Python<'py>, width: i64, height: i64, pixels: &[u8]) -> PyResult<Bound<'py, PyBytes>> {
    let png = py.detach(|| px::rgba_png(width, height, pixels)).map_err(PyValueError::new_err)?;
    Ok(PyBytes::new(py, &png))
}

#[cfg(feature = "python")]
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(rgba_png, m)?)?;
    rs_common::export(m, &["rgba_png"])
}
